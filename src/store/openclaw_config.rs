use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::fs;
#[cfg(any(unix, windows))]
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;
use serde_json::{Map, Value};
use url::{Host, Url};

use crate::env::EnvMeta;

use super::common::{ensure_dir, path_exists, write_file_replacing_path};
use super::gateway_ports::read_port_number;
use super::layout::{EnvPaths, clean_path, derive_env_paths, display_path, resolve_user_home};
use super::openclaw_workspaces::{
    ConfigSegment, OpenClawConfigReadContext, OpenClawConfigReadError, OpenClawWorkspaceInventory,
    OpenClawWorkspaceRuntime, load_effective_openclaw_config, read_openclaw_config_field,
    read_regular_text_file, resolve_effective_openclaw_config_value, resolve_openclaw_config_env,
};

#[cfg(all(test, target_os = "macos"))]
#[path = "openclaw_config_macos_tests.rs"]
mod macos_privacy_tests;

#[derive(Clone, Debug)]
pub(crate) struct OpenClawConfigAudit {
    pub status: String,
    pub issues: Vec<String>,
    /// Config paths under the first root move to the same suffix under the second.
    pub repair_root_rewrite: Option<(PathBuf, PathBuf)>,
    pub repair_workspace: bool,
    pub repair_gateway_port: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OpenClawConfigRewriteOutcome {
    pub cleared_sandbox_origin: bool,
    pub sandbox_port: Option<u32>,
    /// Settings whose home-relative entries migration kept as written
    /// although they may now resolve under the environment root.
    pub kept_home_relative_settings: Vec<KeptHomeRelativeSetting>,
}

/// A setting whose home-relative entries migration kept as written, because
/// spelling them out could change what it means.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct KeptHomeRelativeSetting {
    /// The setting's path, with `[]` for a list entry.
    pub setting: String,
    pub reason: KeptHomeRelativeReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum KeptHomeRelativeReason {
    /// A trust root: spelling it out would change what the environment trusts.
    TrustRoot,
    /// A value under `~/.openclaw` with a `${VAR}` placeholder, which may
    /// lead outside the state directory once substituted.
    Placeholder,
}

pub(crate) fn audit_openclaw_config(
    meta: &EnvMeta,
    known_envs: &[EnvMeta],
    env: &BTreeMap<String, String>,
) -> OpenClawConfigAudit {
    let paths = derive_env_paths(Path::new(&meta.root));
    if !path_exists(&paths.config_path) {
        return OpenClawConfigAudit {
            status: "absent".to_string(),
            issues: Vec::new(),
            repair_root_rewrite: None,
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
                repair_root_rewrite: None,
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
                repair_root_rewrite: None,
                repair_workspace: false,
                repair_gateway_port: false,
            };
        }
    };

    audit_openclaw_config_value(
        meta,
        known_envs,
        &paths,
        &value,
        &clean_path(&resolve_user_home(env)),
    )
}

pub(crate) struct OpenClawConfigRepair {
    config_path: PathBuf,
    pub(crate) value: Value,
}

impl OpenClawConfigRepair {
    pub(crate) fn apply(&self) -> Result<(), String> {
        write_config_value(&self.config_path, &self.value)
    }
}

pub(crate) fn prepare_openclaw_config_repair(
    meta: &EnvMeta,
    known_envs: &[EnvMeta],
    env: &BTreeMap<String, String>,
) -> Result<Option<OpenClawConfigRepair>, String> {
    let paths = derive_env_paths(Path::new(&meta.root));
    let Some(mut value) = read_config_value(&paths.config_path)? else {
        return Ok(None);
    };

    let audit = audit_openclaw_config_value(
        meta,
        known_envs,
        &paths,
        &value,
        &clean_path(&resolve_user_home(env)),
    );
    if audit.status != "drifted" {
        return Ok(None);
    }

    let mut changed = false;
    if let Some((source_root, target_root)) = audit.repair_root_rewrite.as_ref() {
        changed |= rewrite_env_root_paths(&mut value, source_root, target_root);
    }
    if audit.repair_workspace && workspace_field_needs_repair(&value, &paths.root) {
        changed |= rewrite_workspace_field(&mut value, &paths.workspace_dir);
    }
    if audit.repair_gateway_port {
        changed |= rewrite_gateway_port_family(&mut value, meta.gateway_port.unwrap_or_default());
    }

    if !changed {
        return Ok(None);
    }

    Ok(Some(OpenClawConfigRepair {
        config_path: paths.config_path,
        value,
    }))
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

/// Reports whether the config file itself, not an include, sets the Gateway
/// port.
pub(crate) fn openclaw_config_file_sets_gateway_port(paths: &EnvPaths) -> Result<bool, String> {
    Ok(read_config_value(&paths.config_path)?
        .as_ref()
        .and_then(read_gateway_port)
        .is_some())
}

/// Moves a config-set Gateway port, and the ports coupled to it, to
/// `gateway_port`. A config without a Gateway port is left untouched.
pub(crate) fn rewrite_openclaw_config_gateway_port(
    paths: &EnvPaths,
    gateway_port: u32,
) -> Result<(), String> {
    let Some(mut value) = read_config_value(&paths.config_path)? else {
        return Ok(());
    };
    if rewrite_gateway_port_family(&mut value, gateway_port) {
        write_config_value(&paths.config_path, &value)?;
    }
    Ok(())
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

/// `source_home` is the home a plain install expands `~` against.
pub(crate) fn rewrite_openclaw_config_for_migration(
    target_paths: &EnvPaths,
    source_home: &Path,
    source_state_root: &Path,
    canonical_source_state_root: &Path,
    gateway_port: Option<u32>,
    sandbox_origin: Option<&str>,
) -> Result<OpenClawConfigRewriteOutcome, String> {
    let kept = expand_migrated_home_relative_paths(
        target_paths,
        &HomeExpansion {
            home: source_home,
            source_state_dir: source_state_root,
            target_state_dir: &target_paths.state_dir,
        },
    )?;
    rewrite_migrated_plugin_load_paths(
        target_paths,
        source_state_root,
        canonical_source_state_root,
    )?;
    let mut outcome = rewrite_openclaw_config_with_root_mapping(
        target_paths,
        Some((source_state_root, target_paths.state_dir.as_path())),
        gateway_port,
        SandboxOriginPolicy::NewEnvironment(sandbox_origin),
    )?;
    outcome.kept_home_relative_settings = kept.into_iter().collect();
    Ok(outcome)
}

/// A plain install expands `~` against its home, but the environment runs
/// with `OPENCLAW_HOME` set to its root, which OpenClaw expands `~` against
/// instead. Values under `~/.openclaw` already name the copied state
/// directory; every other home-relative path setting is spelled out against
/// the source home so it keeps naming the same location. Absolute values
/// inside the source state directory are then relocated by the later rewrites.
/// Returns the settings whose home-relative entries were kept as written
/// although they may now resolve elsewhere.
fn expand_migrated_home_relative_paths(
    target: &EnvPaths,
    expansion: &HomeExpansion<'_>,
) -> Result<BTreeSet<KeptHomeRelativeSetting>, String> {
    let mut kept = BTreeSet::new();
    if !expansion.home.is_absolute() {
        return Ok(kept);
    }
    let Some(effective) = load_effective_openclaw_config(&target.config_path)? else {
        return Ok(kept);
    };
    let mut scopes = effective.include_scopes;
    scopes.insert(target.config_path.clone(), BTreeSet::from([Vec::new()]));
    for (path, scopes) in scopes {
        let Some(value) = read_config_value(&path)? else {
            continue;
        };
        let mut rewritten = None;
        for mut scope in scopes {
            let mut candidate = value.clone();
            expand_home_relative_paths_in_scope(&mut candidate, &mut scope, expansion, &mut kept);
            if rewritten
                .as_ref()
                .is_some_and(|previous| previous != &candidate)
            {
                return Err(format!(
                    "cannot safely expand home-relative paths in shared OpenClaw include {}; give path settings their own include before migration",
                    display_path(&path)
                ));
            }
            rewritten = Some(candidate);
        }
        if let Some(rewritten) = rewritten.filter(|rewritten| *rewritten != value) {
            write_copied_config_value(&path, &target.state_dir, &rewritten)?;
        }
    }
    Ok(kept)
}

/// The home `~` named in the plain install, and the state directory the
/// later relocation moves absolute values from and to.
struct HomeExpansion<'a> {
    home: &'a Path,
    source_state_dir: &'a Path,
    target_state_dir: &'a Path,
}

/// Expands the home-relative path settings in a config fragment included at
/// `scope`, and records the settings it keeps as written although they may
/// now resolve elsewhere. `$include` directives keep their own relocation.
fn expand_home_relative_paths_in_scope(
    value: &mut Value,
    scope: &mut Vec<ConfigSegment>,
    expansion: &HomeExpansion<'_>,
    kept: &mut BTreeSet<KeptHomeRelativeSetting>,
) -> bool {
    let home = expansion.home;
    match value {
        Value::String(raw) => {
            let Some(rule) = home_relative_path_setting(scope) else {
                return false;
            };
            let (expanded, trims) = match rule {
                HomePathRule::Lexical { trims } => {
                    (expand_home_relative_path(raw, home, trims), trims)
                }
                HomePathRule::Physical => {
                    (expand_physical_home_relative_path(raw, expansion), false)
                }
                HomePathRule::TrustRoot => {
                    let moves = expand_home_relative_path(raw, home, true).is_some()
                        || has_home_relative_placeholder(raw, true);
                    if moves {
                        kept.insert(KeptHomeRelativeSetting {
                            setting: config_setting_name(scope),
                            reason: KeptHomeRelativeReason::TrustRoot,
                        });
                    }
                    return false;
                }
            };
            let Some(expanded) = expanded else {
                // A placeholder under `~/.openclaw` can still lead outside it.
                if has_home_relative_placeholder(raw, trims) {
                    kept.insert(KeptHomeRelativeSetting {
                        setting: config_setting_name(scope),
                        reason: KeptHomeRelativeReason::Placeholder,
                    });
                }
                return false;
            };
            *raw = expanded;
            true
        }
        Value::Array(values) => {
            let mut changed = false;
            for (index, nested) in values.iter_mut().enumerate() {
                scope.push(ConfigSegment::Index(index));
                changed |= expand_home_relative_paths_in_scope(nested, scope, expansion, kept);
                scope.pop();
            }
            changed
        }
        Value::Object(values) => {
            let mut changed = false;
            for (key, nested) in values.iter_mut().filter(|(key, _)| *key != "$include") {
                scope.push(ConfigSegment::Key(key.clone()));
                changed |= expand_home_relative_paths_in_scope(nested, scope, expansion, kept);
                scope.pop();
            }
            changed
        }
        _ => false,
    }
}

/// How OpenClaw resolves a home-relative setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HomePathRule {
    /// The leading `~` is replaced, after trimming when `trims` is set, and
    /// the path is resolved lexically.
    Lexical { trims: bool },
    /// The leading `~` is replaced and the path is resolved through links
    /// (`realpath`), so a link before `..` decides where it leads.
    Physical,
    /// A trust root. Spelling it out would change what the environment
    /// trusts, so it stays as written and is reported.
    TrustRoot,
}

/// The setting at `scope` as users write it, with `[]` for a list entry.
fn config_setting_name(scope: &[ConfigSegment]) -> String {
    let mut name = String::new();
    for segment in scope {
        match segment {
            ConfigSegment::Key(key) => {
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(key);
            }
            ConfigSegment::Index(_) => name.push_str("[]"),
        }
    }
    name
}

/// Settings whose own OpenClaw consumers expand `~` against `OPENCLAW_HOME`
/// with OpenClaw's shared resolver although its runtime config rule does
/// not, with how each resolves it. `*` stands for any one object key and
/// `[]` for a list entry. Derived from the consumers at OpenClaw
/// a7fe06b17a9. Settings their consumers expand against the OS home (string
/// `memory.search.extraPaths` entries, `messages.usageTemplate`, the
/// voice-call store and the file-transfer path lists) keep their meaning,
/// because `HOME` is unchanged in the environment.
const CONSUMER_HOME_PATH_SETTINGS: &[(&[&str], HomePathRule)] = &[
    // src/skills/loading/workspace-skill-sources.ts
    (
        &["skills", "load", "extraDirs", "[]"],
        HomePathRule::Lexical { trims: true },
    ),
    // src/hooks/workspace.ts
    (
        &["hooks", "internal", "load", "extraDirs", "[]"],
        HomePathRule::Lexical { trims: true },
    ),
    // src/config/sessions/paths.ts expands `~` without trimming first.
    (
        &["session", "store"],
        HomePathRule::Lexical { trims: false },
    ),
    // src/cron/store/paths.ts
    (&["cron", "store"], HomePathRule::Lexical { trims: true }),
    // src/agents/agent-scope-config.ts
    (
        &["agents", "defaults", "cwd"],
        HomePathRule::Lexical { trims: true },
    ),
    (
        &["agents", "entries", "*", "cwd"],
        HomePathRule::Lexical { trims: true },
    ),
    (
        &["agents", "list", "[]", "cwd"],
        HomePathRule::Lexical { trims: true },
    ),
    // src/node-host/node-worker-device-folder.ts: `expandHomePrefix`, then
    // `fs.realpathSync.native`, without trimming.
    (
        &["nodeHost", "workerRuns", "folders", "[]"],
        HomePathRule::Physical,
    ),
    // src/skills/loading/symlink-targets.ts and src/security/install-policy.ts
    (
        &["skills", "load", "allowSymlinkTargets", "[]"],
        HomePathRule::TrustRoot,
    ),
    (
        &["security", "installPolicy", "exec", "trustedDirs", "[]"],
        HomePathRule::TrustRoot,
    ),
];

/// How OpenClaw resolves a leading `~` in the string setting at `scope`, if
/// it expands one. OpenClaw's runtime config (`materializeRuntimeConfig` in
/// `src/config/materialize.ts`) expands keys ending in `dir`, `path`,
/// `paths`, `file`, `root` or `workspace`, the keys `paths` and
/// `pathPrepend`, and the direct entries of `paths` and `pathPrepend` lists;
/// [`CONSUMER_HOME_PATH_SETTINGS`] lists the settings that are expanded by
/// their own consumers. Other strings, such as message text, stay as written.
fn home_relative_path_setting(scope: &[ConfigSegment]) -> Option<HomePathRule> {
    if let Some((_, rule)) = CONSUMER_HOME_PATH_SETTINGS
        .iter()
        .find(|(pattern, _)| config_scope_matches(scope, pattern))
    {
        return Some(*rule);
    }
    const PATH_LISTS: [&str; 2] = ["paths", "pathPrepend"];
    const PATH_KEY_SUFFIXES: [&str; 6] = ["dir", "path", "paths", "file", "root", "workspace"];
    let lexical = HomePathRule::Lexical { trims: true };
    match scope {
        [.., ConfigSegment::Key(list), ConfigSegment::Index(_)] => {
            PATH_LISTS.contains(&list.as_str()).then_some(lexical)
        }
        [.., ConfigSegment::Key(key)] => {
            let lower = key.to_ascii_lowercase();
            (PATH_LISTS.contains(&key.as_str())
                || PATH_KEY_SUFFIXES
                    .iter()
                    .any(|suffix| lower.ends_with(suffix)))
            .then_some(lexical)
        }
        _ => None,
    }
}

fn config_scope_matches(scope: &[ConfigSegment], pattern: &[&str]) -> bool {
    scope.len() == pattern.len()
        && scope
            .iter()
            .zip(pattern)
            .all(|(segment, expected)| match (segment, *expected) {
                (ConfigSegment::Index(_), "[]") => true,
                (ConfigSegment::Key(_), "*") => true,
                (ConfigSegment::Key(key), expected) => key == expected,
                _ => false,
            })
}

/// The characters JavaScript's `String.prototype.trim` removes: its white
/// space (tab, vertical tab, form feed, BOM and the `Zs` space separators)
/// and line terminators. Rust's `str::trim` differs, for example on U+0085.
fn is_ecmascript_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn is_home_relative(value: &str) -> bool {
    value == "~" || value.starts_with("~/") || value.starts_with("~\\")
}

/// Whether OpenClaw would read `raw` as a home-relative value containing a
/// `${VAR}` placeholder.
fn has_home_relative_placeholder(raw: &str, trims: bool) -> bool {
    let value = if trims {
        raw.trim_matches(is_ecmascript_whitespace)
    } else {
        raw
    };
    is_home_relative(value) && value.contains("${")
}

/// Splits a home-relative value before the separator that precedes its
/// first `${VAR}` placeholder. OpenClaw substitutes placeholders before it
/// resolves the path, so a placeholder's value can still add `..` or name
/// any directory; only the literal directories before it are known here.
fn split_before_placeholder(value: &str) -> (&str, &str) {
    let Some(placeholder) = value.find("${") else {
        return (value, "");
    };
    // `~` is followed by a separator, so the split never falls before it.
    let split = value[..placeholder]
        .rfind(std::path::is_separator)
        .unwrap_or(1);
    value.split_at(split)
}

fn expand_home_relative_path(raw: &str, home: &Path, trims: bool) -> Option<String> {
    let value = if trims {
        raw.trim_matches(is_ecmascript_whitespace)
    } else {
        raw
    };
    if !is_home_relative(value) {
        return None;
    }
    // Expand as OpenClaw does: replace the leading `~`, then resolve `.` and
    // `..` lexically, so later rewrites cannot follow links OpenClaw ignores.
    // A placeholder and what follows it stay as written: OpenClaw resolves
    // them after substituting the placeholder, exactly as it resolves the
    // same value spelled from the home.
    let (literal, mut rest) = split_before_placeholder(value);
    let mut expanded = home.as_os_str().to_os_string();
    expanded.push(&literal[1..]);
    let expanded = clean_path(Path::new(&expanded));
    if expanded.starts_with(home.join(".openclaw")) {
        return None;
    }
    let expanded = expanded.to_str()?;
    if expanded.ends_with(std::path::is_separator) {
        // The literal part resolved to a filesystem root.
        rest = rest.strip_prefix(std::path::is_separator).unwrap_or(rest);
    }
    Some(format!("{expanded}{rest}"))
}

/// Expands a setting OpenClaw resolves through links. Only the leading `~`
/// is replaced: the consumer follows each link before the `..` after it,
/// which removing `..` here would change. A value under `~/.openclaw` with
/// no `..` already names the copied state directory and stays as written.
/// The later relocation of absolute state paths follows existing links as
/// the consumer does, but it steps over a missing directory the consumer
/// would reject. An entry it would move into the copied state although the
/// source path does not resolve stays as written, so it keeps resolving
/// through the copy of that missing directory.
fn expand_physical_home_relative_path(raw: &str, expansion: &HomeExpansion<'_>) -> Option<String> {
    let home = expansion.home;
    if !is_home_relative(raw) {
        return None;
    }
    let (literal, rest) = split_before_placeholder(raw);
    let mut literal_path = home.as_os_str().to_os_string();
    literal_path.push(&literal[1..]);
    let literal_path = Path::new(&literal_path);
    if literal_path.starts_with(home.join(".openclaw"))
        && (!rest.is_empty()
            || !literal_path
                .components()
                .any(|component| component == std::path::Component::ParentDir))
    {
        return None;
    }
    let expanded = format!("{}{}", home.to_str()?, &raw[1..]);
    let mut relocated = expanded.clone();
    if rewrite_env_root_string(
        &mut relocated,
        expansion.source_state_dir,
        expansion.target_state_dir,
    ) && fs::canonicalize(&expanded).is_err()
    {
        return None;
    }
    Some(expanded)
}

/// Moves a Gateway port that a copied `$include` supplies to `gateway_port`,
/// with the MCP app sandbox port and loopback origin that follow it in
/// `config`, the target's root config after its include directives point at
/// the copies. A port the root config sets itself overrides its includes and
/// is left to the root's own rewrite.
fn rewrite_included_gateway_port(
    config: &mut Value,
    target: &EnvPaths,
    gateway_port: u32,
) -> Result<bool, String> {
    if read_gateway_port(config).is_some() || !value_contains_include(config) {
        return Ok(false);
    }
    let effective = resolve_effective_openclaw_config_value(&target.config_path, config.clone())?;
    let Some(source_port) = read_gateway_port(&effective.value) else {
        return Ok(false);
    };
    // A file reached through several paths, such as a symlinked include, is
    // one include: rewriting it changes every place that reads it, so its
    // scopes are checked together.
    let mut includes = BTreeMap::<PathBuf, (Vec<PathBuf>, BTreeSet<Vec<ConfigSegment>>)>::new();
    for (path, scopes) in effective.include_scopes {
        let file = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let (paths, file_scopes) = includes.entry(file).or_default();
        paths.push(path);
        file_scopes.extend(scopes);
    }
    for (paths, scopes) in includes.into_values() {
        let path = &paths[0];
        let shared_include = || {
            format!(
                "cannot give the new environment its own Gateway port because OpenClaw include {} sets gateway.port and is also included elsewhere in the config; give gateway.port its own include before creating a new OCM environment",
                display_path(path)
            )
        };
        let Some(value) = read_config_value(path)? else {
            continue;
        };
        let mut rewritten = None;
        for scope in scopes {
            let mut candidate = value.clone();
            rewrite_gateway_port_in_scope(&mut candidate, &scope, gateway_port);
            if rewritten
                .as_ref()
                .is_some_and(|previous| previous != &candidate)
            {
                return Err(shared_include());
            }
            rewritten = Some(candidate);
        }
        if let Some(rewritten) = rewritten.filter(|rewritten| *rewritten != value) {
            // Every path reads it as the Gateway port. Write the file itself,
            // so the paths that link to it keep reading it.
            let file_path = if paths.len() == 1 {
                Some(path)
            } else {
                paths.iter().find(|path| {
                    fs::symlink_metadata(path)
                        .is_ok_and(|metadata| !metadata.file_type().is_symlink())
                })
            };
            let Some(file_path) = file_path else {
                return Err(shared_include());
            };
            write_copied_config_value(file_path, &target.state_dir, &rewritten)?;
        }
    }
    Ok(rewrite_port_coupled_mcp_app_sandbox(
        config,
        source_port,
        gateway_port,
    ))
}

/// Rewrites `gateway.port` in a config fragment included at `scope`.
fn rewrite_gateway_port_in_scope(value: &mut Value, scope: &[ConfigSegment], gateway_port: u32) {
    const GATEWAY_PORT: [&str; 2] = ["gateway", "port"];
    if scope.len() > GATEWAY_PORT.len() || scope.iter().zip(GATEWAY_PORT).any(|(a, b)| !a.is(b)) {
        return;
    }
    let mut port = value;
    for key in &GATEWAY_PORT[scope.len()..] {
        let Some(child) = port.get_mut(*key) else {
            return;
        };
        port = child;
    }
    if port.is_number() {
        *port = Value::from(gateway_port);
    }
}

fn rewrite_migrated_plugin_load_paths(
    target: &EnvPaths,
    source: &Path,
    canonical_source: &Path,
) -> Result<(), String> {
    let Some(effective) = load_effective_openclaw_config(&target.config_path)? else {
        return Ok(());
    };
    let mut scopes = effective.include_scopes;
    scopes.insert(target.config_path.clone(), BTreeSet::from([Vec::new()]));
    for (path, scopes) in scopes {
        let Some(value) = read_config_value(&path)? else {
            continue;
        };
        let mut rewritten = None;
        for mut scope in scopes {
            let mut candidate = value.clone();
            rewrite_migrated_plugin_paths_in_scope(
                &mut candidate,
                &mut scope,
                source,
                canonical_source,
                &target.state_dir,
            );
            let mut final_candidate = candidate.clone();
            rewrite_env_root_paths(&mut final_candidate, source, &target.state_dir);
            if rewritten
                .as_ref()
                .is_some_and(|(_, previous)| previous != &final_candidate)
            {
                return Err(format!(
                    "cannot safely rewrite plugin load paths in shared OpenClaw include {}; separate plugin-load and other include consumers before migration",
                    display_path(&path)
                ));
            }
            rewritten = Some((candidate, final_candidate));
        }
        if let Some((rewritten, _)) = rewritten.filter(|(rewritten, _)| *rewritten != value) {
            write_copied_config_value(&path, &target.state_dir, &rewritten)?;
        }
    }
    Ok(())
}

fn rewrite_migrated_plugin_paths_in_scope(
    value: &mut Value,
    scope: &mut Vec<ConfigSegment>,
    source: &Path,
    canonical_source: &Path,
    target: &Path,
) {
    if scope.len() == 4
        && scope
            .iter()
            .zip(["plugins", "load", "paths"])
            .all(|(segment, name)| segment.is(name))
        && let Some(raw) = value.as_str()
    {
        let path = Path::new(raw);
        let mut admitted = raw.to_string();
        // Resolve only the two admitted identities. Copying preserves absolute
        // links, so lexical replacement alone can keep loading source bytes.
        if (path.starts_with(source) || path.starts_with(canonical_source))
            && rewrite_env_root_string(&mut admitted, source, target)
            && let Ok(physical) = fs::canonicalize(path)
            && let Ok(relative) = physical.strip_prefix(canonical_source)
        {
            *value = Value::String(display_path(&target.join(relative)));
        }
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                scope.push(ConfigSegment::Key(key.clone()));
                rewrite_migrated_plugin_paths_in_scope(
                    child,
                    scope,
                    source,
                    canonical_source,
                    target,
                );
                scope.pop();
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter_mut().enumerate() {
                scope.push(ConfigSegment::Index(index));
                rewrite_migrated_plugin_paths_in_scope(
                    child,
                    scope,
                    source,
                    canonical_source,
                    target,
                );
                scope.pop();
            }
        }
        _ => {}
    }
}

pub(crate) fn rewrite_external_workspace_paths_for_migration(
    config_path: &Path,
    default_workspace: Option<&Path>,
    agent_workspaces: &BTreeMap<String, PathBuf>,
    resolved_agent_ids: &BTreeMap<String, String>,
) -> Result<bool, String> {
    if default_workspace.is_none() && agent_workspaces.is_empty() {
        return Ok(false);
    }
    let mut value = read_config_value(config_path)?.unwrap_or_else(|| json!({}));
    let implicit_main = resolved_agent_ids.is_empty();
    let object = value
        .as_object_mut()
        .ok_or("cannot rewrite migrated workspace in a non-object OpenClaw config")?;
    let agents = ensure_object_field(object, "agents");

    let mut changed = false;
    if let Some(default_workspace) = default_workspace {
        changed |= set_workspace_path(agents.get_mut("defaults"), default_workspace);
    }
    // With no authored agent entry, OpenClaw's implicit main agent consumes
    // defaults.workspace. Do not set this fallback for an authored agent: that
    // would also change the implicit workspace selection of secondary agents.
    if implicit_main && let Some(workspace) = agent_workspaces.get("main") {
        ensure_object_field(agents, "defaults");
        changed |= set_workspace_path(agents.get_mut("defaults"), workspace);
    }
    if let Some(entries) = agents.get_mut("entries").and_then(Value::as_object_mut) {
        for (agent_id, entry) in entries {
            let Some(workspace) = resolved_agent_ids
                .get(agent_id)
                .and_then(|id| agent_workspaces.get(id))
            else {
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
            let Some(workspace) = resolved_agent_ids
                .get(agent_id)
                .and_then(|id| agent_workspaces.get(id))
            else {
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

pub(super) fn rewrite_archived_openclaw_config(
    target_paths: &EnvPaths,
    source_roots: &[&Path],
    canonical_source_root: Option<&Path>,
    gateway_port: Option<u32>,
    sandbox_origin: Option<&str>,
) -> Result<OpenClawConfigRewriteOutcome, String> {
    let source_config_roots = source_roots
        .iter()
        .map(|root| derive_env_paths(root).state_dir)
        .collect::<Vec<_>>();
    rewrite_openclaw_config_with_path_rewriter(
        target_paths,
        gateway_port,
        SandboxOriginPolicy::NewEnvironment(sandbox_origin),
        !source_roots.is_empty(),
        |value| {
            rewrite_config_paths_with_archive_includes(
                value,
                &source_config_roots,
                &target_paths.state_dir,
                &|value| {
                    rewrite_env_root_paths_from_roots(
                        value,
                        source_roots,
                        canonical_source_root,
                        &target_paths.root,
                    )
                },
            )
        },
    )
}

fn rewrite_openclaw_config_with_root_mapping(
    target_paths: &EnvPaths,
    root_mapping: Option<(&Path, &Path)>,
    gateway_port: Option<u32>,
    sandbox_origin_policy: SandboxOriginPolicy<'_>,
) -> Result<OpenClawConfigRewriteOutcome, String> {
    let source_roots: Vec<&Path> = root_mapping.map(|(source, _)| source).into_iter().collect();
    let target_root = root_mapping
        .map(|(_, target)| target)
        .unwrap_or(&target_paths.root);
    rewrite_openclaw_config_with_path_rewriter(
        target_paths,
        gateway_port,
        sandbox_origin_policy,
        matches!(
            sandbox_origin_policy,
            SandboxOriginPolicy::NewEnvironment(_)
        ) && root_mapping.is_some(),
        |value| rewrite_env_root_paths_from_roots(value, &source_roots, None, target_root),
    )
}

fn rewrite_openclaw_config_with_path_rewriter(
    target_paths: &EnvPaths,
    gateway_port: Option<u32>,
    sandbox_origin_policy: SandboxOriginPolicy<'_>,
    rewrite_includes: bool,
    rewrite_paths: impl Fn(&mut Value) -> bool,
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
    changed |= rewrite_paths(&mut value);
    changed |= rewrite_workspace_field_if_env_scoped(
        &mut value,
        &target_paths.root,
        &target_paths.workspace_dir,
    );
    if let Some(gateway_port) = gateway_port {
        if matches!(
            sandbox_origin_policy,
            SandboxOriginPolicy::NewEnvironment(_)
        ) {
            changed |= rewrite_included_gateway_port(&mut value, target_paths, gateway_port)?;
        }
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

    if changed || config_is_symlink {
        write_config_value(&target_paths.config_path, &value)?;
    }

    if rewrite_includes {
        rewrite_openclaw_config_includes_with_path_rewriter(
            &target_paths.config_path,
            &target_paths.state_dir,
            rewrite_paths,
        )?;
    }
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

pub(super) fn rewrite_copied_include_directives(
    include_paths: &[PathBuf],
    source_roots: &[&Path],
    target_root: &Path,
) -> Result<(), String> {
    let source_config_roots = source_roots
        .iter()
        .map(|root| derive_env_paths(root).state_dir)
        .collect::<Vec<_>>();
    let target_config_root = derive_env_paths(target_root).state_dir;
    rewrite_copied_config_include_directives(
        include_paths,
        &source_config_roots,
        &target_config_root,
    )
}

pub(crate) fn rewrite_copied_config_include_directives(
    include_paths: &[PathBuf],
    source_config_roots: &[PathBuf],
    target_config_root: &Path,
) -> Result<(), String> {
    for path in include_paths {
        let mut value = read_config_value(path)?
            .ok_or_else(|| format!("copied OpenClaw include is missing: {}", display_path(path)))?;
        if rewrite_config_paths_with_archive_includes(
            &mut value,
            source_config_roots,
            target_config_root,
            &|_| false,
        ) {
            write_copied_config_value(path, target_config_root, &value)?;
        }
    }
    Ok(())
}

fn rewrite_config_paths_with_archive_includes(
    value: &mut Value,
    source_config_roots: &[PathBuf],
    target_config_root: &Path,
    rewrite_other: &impl Fn(&mut Value) -> bool,
) -> bool {
    match value {
        Value::Object(object) => object.iter_mut().fold(false, |changed, (key, value)| {
            let rewritten = if key == "$include" {
                rewrite_include_path(value, source_config_roots, target_config_root)
            } else {
                rewrite_config_paths_with_archive_includes(
                    value,
                    source_config_roots,
                    target_config_root,
                    rewrite_other,
                )
            };
            changed || rewritten
        }),
        Value::Array(values) => values.iter_mut().fold(false, |changed, value| {
            rewrite_config_paths_with_archive_includes(
                value,
                source_config_roots,
                target_config_root,
                rewrite_other,
            ) || changed
        }),
        _ => rewrite_other(value),
    }
}

fn rewrite_include_path(
    value: &mut Value,
    source_config_roots: &[PathBuf],
    target_config_root: &Path,
) -> bool {
    match value {
        Value::String(raw) => {
            let Some(mapped) = super::openclaw_workspaces::map_archived_config_include(
                Path::new(raw),
                source_config_roots,
                target_config_root,
            ) else {
                return false;
            };
            let mapped = display_path(&mapped);
            if *raw == mapped {
                return false;
            }
            *raw = mapped;
            true
        }
        Value::Array(values) => values.iter_mut().fold(false, |changed, value| {
            rewrite_include_path(value, source_config_roots, target_config_root) || changed
        }),
        _ => false,
    }
}

pub(crate) fn rewrite_openclaw_config_includes_for_target(
    config_path: &Path,
    state_dir: &Path,
    source_root: &Path,
    target_root: &Path,
) -> Result<bool, String> {
    rewrite_openclaw_config_includes_with_path_rewriter(config_path, state_dir, |value| {
        rewrite_env_root_paths_from_roots(value, &[source_root], None, target_root)
    })
}

fn rewrite_openclaw_config_includes_with_path_rewriter(
    config_path: &Path,
    state_dir: &Path,
    rewrite_paths: impl Fn(&mut Value) -> bool,
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
        if rewrite_paths(&mut value) {
            write_copied_config_value(&include_path, state_dir, &value)?;
            changed = true;
        }
    }
    Ok(changed)
}

pub(crate) fn rewrite_included_agent_directories_for_target(
    config_path: &Path,
    source_paths: &OpenClawWorkspaceInventory,
    source_root: &Path,
    target_root: &Path,
) -> Result<bool, String> {
    let replacements = source_paths
        .agent_directory_templates()
        .iter()
        .filter(|(template, _)| workspace_uses_changing_runtime_identity(template))
        .map(|(template, path)| {
            let relative = path
                .strip_prefix(source_root)
                .map_err(|error| error.to_string())?;
            Ok((
                template.clone(),
                display_path(&clean_path(&target_root.join(relative))),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    if replacements.is_empty() {
        return Ok(false);
    }
    let Some(effective) = load_effective_openclaw_config(config_path)? else {
        return Ok(false);
    };
    let mut scopes = effective.include_scopes;
    scopes.insert(config_path.to_path_buf(), BTreeSet::from([Vec::new()]));
    let mut changed = false;
    for (path, scopes) in scopes {
        let Some(value) = read_config_value(&path)? else {
            continue;
        };
        let mut rewritten = None;
        for mut scope in scopes {
            let mut candidate = value.clone();
            rewrite_agent_directory_templates(&mut candidate, &mut scope, &replacements);
            if rewritten
                .as_ref()
                .is_some_and(|previous| previous != &candidate)
            {
                return Err(format!(
                    "cannot safely rewrite identity-bound agent directory in shared OpenClaw include {}; separate agent and non-agent include consumers before simulation",
                    display_path(&path)
                ));
            }
            rewritten = Some(candidate);
        }
        if let Some(rewritten) = rewritten.filter(|rewritten| *rewritten != value) {
            reject_protected_config_write(&derive_env_paths(target_root).state_dir, &path)?;
            write_config_value(&path, &rewritten)?;
            changed = true;
        }
    }
    Ok(changed)
}

fn rewrite_agent_directory_templates(
    value: &mut Value,
    scope: &mut Vec<ConfigSegment>,
    replacements: &BTreeMap<String, String>,
) -> bool {
    if scope.len() == 4
        && scope[0].is("agents")
        && (scope[1].is("entries") || scope[1].is("list"))
        && scope[3].is("agentDir")
        && let Some(replacement) = value.as_str().and_then(|raw| replacements.get(raw))
    {
        *value = Value::String(replacement.clone());
        return true;
    }
    let mut changed = false;
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                scope.push(ConfigSegment::Key(key.clone()));
                changed |= rewrite_agent_directory_templates(child, scope, replacements);
                scope.pop();
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter_mut().enumerate() {
                scope.push(ConfigSegment::Index(index));
                changed |= rewrite_agent_directory_templates(child, scope, replacements);
                scope.pop();
            }
        }
        _ => {}
    }
    changed
}

pub(crate) fn rewrite_identity_bound_workspace_paths_for_target(
    config_path: &Path,
    source_workspaces: &OpenClawWorkspaceInventory,
    source_root: &Path,
    target_root: &Path,
    source_agent_root: &Path,
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
            changed |= rewrite_identity_bound_agent_directory(
                entry,
                source_workspaces.agent_directory(agent_id),
                source_root,
                source_agent_root,
                target_root,
            )?;
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
            let Some(agent_id) = entry.get("id").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            changed |= rewrite_identity_bound_agent_directory(
                entry,
                source_workspaces.agent_directory(&agent_id),
                source_root,
                source_agent_root,
                target_root,
            )?;
            let identity_bound = entry
                .get("workspace")
                .and_then(Value::as_str)
                .is_some_and(workspace_uses_changing_runtime_identity);
            if !identity_bound {
                continue;
            }
            let Some(source_workspace) = source_workspaces.agent_workspace(&agent_id) else {
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

fn rewrite_identity_bound_agent_directory(
    entry: &mut Map<String, Value>,
    source_directory: Option<&Path>,
    source_root: &Path,
    source_agent_root: &Path,
    target_root: &Path,
) -> Result<bool, String> {
    if !entry
        .get("agentDir")
        .and_then(Value::as_str)
        .is_some_and(workspace_uses_changing_runtime_identity)
    {
        return Ok(false);
    }
    let Some(source_directory) = source_directory else {
        return Ok(false);
    };
    let relative = source_directory.strip_prefix(source_root).or_else(|_| source_directory.strip_prefix(source_agent_root)).map_err(|_| {
        format!("cannot rewrite identity-bound OpenClaw agent directory outside the source environment root: {}", display_path(source_directory))
    })?;
    entry.insert(
        "agentDir".to_string(),
        Value::String(display_path(&clean_path(&target_root.join(relative)))),
    );
    Ok(true)
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
    prepare_minimum_local_openclaw_config(
        target_paths,
        gateway_port,
        &OpenClawConfigReadContext::default(),
    )?
    .apply()
}

pub(crate) struct MinimumLocalConfig {
    paths: EnvPaths,
    value: Value,
    changed: bool,
    original: Option<String>,
}

impl MinimumLocalConfig {
    pub(crate) fn apply(self) -> Result<(), String> {
        if read_config_raw(&self.paths.config_path)? != self.original {
            return Err(
                "OpenClaw configuration changed during minimum setup; retry without overwriting it"
                    .to_string(),
            );
        }
        ensure_local_state_paths(&self.paths)?;
        if self.changed {
            write_config_value(&self.paths.config_path, &self.value)?;
        }
        Ok(())
    }
}

pub(crate) fn prepare_minimum_local_openclaw_config(
    target_paths: &EnvPaths,
    gateway_port: u32,
    context: &OpenClawConfigReadContext,
) -> Result<MinimumLocalConfig, String> {
    let original_raw = read_config_raw(&target_paths.config_path)?;
    let original: Option<Value> = original_raw
        .as_deref()
        .map(json5::from_str)
        .transpose()
        .map_err(|error| error.to_string())?;
    let missing_paths: Vec<&[&str]> = [
        ("/gateway/mode", &["gateway", "mode"][..]),
        ("/gateway/bind", &["gateway", "bind"][..]),
        (
            "/agents/defaults/workspace",
            &["agents", "defaults", "workspace"][..],
        ),
    ]
    .into_iter()
    .filter(|(pointer, _)| {
        original
            .as_ref()
            .and_then(|value| value.pointer(pointer))
            .is_none()
    })
    .map(|(_, path)| path)
    .collect();
    let mut effective = json!({});
    for path in missing_paths {
        if let Some(value) = read_openclaw_config_field(&target_paths.config_path, path, context)
            .value
            .map_err(|error| {
                format!("cannot prepare minimum setting {}: {error}", path.join("."))
            })?
        {
            let mut parent = &mut effective;
            for key in &path[..path.len() - 1] {
                if parent.get(*key).is_none() {
                    parent[*key] = json!({});
                }
                parent = &mut parent[*key];
            }
            parent[path[path.len() - 1]] = value;
        }
    }
    let value = minimum_local_config_value(
        target_paths,
        gateway_port,
        original.clone().unwrap_or_else(|| json!({})),
        &effective,
    )?;
    Ok(MinimumLocalConfig {
        paths: target_paths.clone(),
        changed: original.as_ref() != Some(&value),
        value,
        original: original_raw,
    })
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
        let mut value =
            minimum_local_config_value(target_paths, gateway_port, json!({}), &json!({}))?;
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
    effective: &Value,
) -> Result<Value, String> {
    let workspace = display_path(&target_paths.workspace_dir);
    if !value.is_object() {
        value = json!({});
    }

    let mode_missing =
        value.pointer("/gateway/mode").is_none() && effective.pointer("/gateway/mode").is_none();
    let bind_missing =
        value.pointer("/gateway/bind").is_none() && effective.pointer("/gateway/bind").is_none();
    let workspace_missing = value.pointer("/agents/defaults/workspace").is_none()
        && effective.pointer("/agents/defaults/workspace").is_none();
    let root = value
        .as_object_mut()
        .ok_or_else(|| "OpenClaw config root must be an object".to_string())?;

    let gateway = ensure_object_field(root, "gateway");
    // Root siblings override includes. Add defaults only when the effective
    // configuration lacks them, leaving authored include values at their source.
    if mode_missing {
        gateway.insert("mode".to_string(), Value::String("local".to_string()));
    }
    if bind_missing {
        gateway.insert("bind".to_string(), Value::String("loopback".to_string()));
    }
    gateway.insert(
        "port".to_string(),
        Value::Number(serde_json::Number::from(gateway_port)),
    );

    if workspace_missing {
        let agents = ensure_object_field(root, "agents");
        let defaults = ensure_object_field(agents, "defaults");
        defaults.insert("workspace".to_string(), Value::String(workspace));
    }

    Ok(value)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GatewayReadinessHost {
    Address(std::net::Ipv4Addr),
    Tailnet,
}

/// Resolve only the bind settings needed by readiness, using launch authority.
/// The readiness owner verifies that a selected address belongs to this host.
pub(crate) fn gateway_readiness_host(
    meta: &EnvMeta,
    port: u32,
    run_dir: &Path,
    env: &BTreeMap<String, String>,
    resolve_native: impl FnOnce() -> Result<Option<Value>, String>,
) -> Result<GatewayReadinessHost, String> {
    let paths = derive_env_paths(Path::new(&meta.root));
    let context = OpenClawConfigReadContext::for_launch(env, run_dir)?;
    let local = (|| -> Result<Value, OpenClawConfigReadError> {
        let mut gateway = Map::new();
        for field in ["bind", "customBindHost"] {
            if field == "customBindHost"
                && gateway.get("bind").and_then(Value::as_str) != Some("custom")
                && !gateway
                    .get("bind")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v.contains("${"))
            {
                break;
            }
            if let Some(value) =
                read_openclaw_config_field(&paths.config_path, &["gateway", field], &context)
                    .value?
            {
                gateway.insert(field.to_string(), value);
            }
        }
        Ok(json!({"gateway": gateway}))
    })();
    let templated = local.as_ref().ok().is_some_and(|config| {
        ["bind", "customBindHost"].iter().any(|field| {
            config["gateway"][field]
                .as_str()
                .is_some_and(|v| v.contains("${"))
        })
    });
    let native = if matches!(&local, Err(OpenClawConfigReadError::OutsideAuthority(_))) || templated
    {
        resolve_native()?
    } else {
        None
    };
    let config = match native {
        Some(gateway) => json!({"gateway": gateway}),
        None => {
            let mut config = match local {
                Ok(config) => config,
                // Preserve opaque launchers' existing loopback destination.
                Err(OpenClawConfigReadError::OutsideAuthority(_)) => {
                    return Ok(GatewayReadinessHost::Address(std::net::Ipv4Addr::LOCALHOST));
                }
                Err(error) => return Err(error.to_string()),
            };
            if templated {
                if let Some(values) =
                    read_openclaw_config_field(&paths.config_path, &["env"], &context)
                        .value
                        .map_err(|error| error.to_string())?
                {
                    config["env"] = values;
                }
                config = resolve_openclaw_config_env(
                    config,
                    env,
                    &paths.openclaw_home,
                    &paths.state_dir,
                    &paths.config_path,
                    OpenClawWorkspaceRuntime::for_env(&meta.name, Some(port)),
                );
            }
            config
        }
    };
    let gateway = &config["gateway"];
    if gateway.get("bind").and_then(Value::as_str) == Some("custom") {
        // Native custom binds accept IPv4 literals only; never resolve DNS.
        let host = gateway
            .get("customBindHost")
            .and_then(Value::as_str)
            .and_then(|host| host.trim().parse::<std::net::Ipv4Addr>().ok())
            .ok_or("Gateway custom bind host must be an IPv4 address")?;
        return Ok(GatewayReadinessHost::Address(if host.is_unspecified() {
            std::net::Ipv4Addr::LOCALHOST
        } else {
            host
        }));
    }
    if gateway.get("bind").and_then(Value::as_str) == Some("tailnet") {
        return Ok(GatewayReadinessHost::Tailnet);
    }
    Ok(GatewayReadinessHost::Address(std::net::Ipv4Addr::LOCALHOST))
}

/// Loopback Gateway URL for a launch context, using readiness's TLS decision.
/// Display callers never run native config lookups; observed TLS selects HTTPS.
pub(crate) fn gateway_url_for_launch(
    meta: &EnvMeta,
    port: u32,
    run_dir: &Path,
    env: &BTreeMap<String, String>,
) -> String {
    let mut tls = false;
    // The certificate is irrelevant here; only the transport decision is used.
    let _ = gateway_readiness_certificate(meta, port, run_dir, env, &mut tls, || Ok(None));
    loopback_gateway_url(port, tls)
}

pub(crate) fn loopback_gateway_url(port: u32, tls: bool) -> String {
    format!("{}://127.0.0.1:{port}", if tls { "https" } else { "http" })
}

/// Resolve only public TLS material; the Gateway owns key loading and generation.
pub(crate) fn gateway_readiness_certificate(
    meta: &EnvMeta,
    port: u32,
    run_dir: &Path,
    env: &BTreeMap<String, String>,
    known_tls: &mut bool,
    resolve_native: impl FnOnce() -> Result<Option<Value>, String>,
) -> Result<Option<Vec<u8>>, String> {
    let paths = derive_env_paths(Path::new(&meta.root));
    let context = OpenClawConfigReadContext::for_launch(env, run_dir)?;
    let enabled =
        read_openclaw_config_field(&paths.config_path, &["gateway", "tls", "enabled"], &context);
    // A fully resolved false may override an earlier true in an include array.
    // Partial resolution cannot discard that evidence and choose plaintext.
    let partial_tls = enabled.value.is_err() && enabled.observed_true;
    let local = (|| -> Result<Value, OpenClawConfigReadError> {
        let enabled = match enabled.value? {
            None | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            Some(_) => {
                return Err(OpenClawConfigReadError::Invalid(
                    "Gateway TLS enabled setting must be a boolean".to_string(),
                ));
            }
        };
        if !enabled {
            return Ok(json!({"gateway": {"tls": {"enabled": false}}}));
        }
        *known_tls = true;
        let certificate = read_openclaw_config_field(
            &paths.config_path,
            &["gateway", "tls", "certPath"],
            &context,
        )
        .value?;
        let mut config = json!({"gateway": {"tls": {"enabled": true}}});
        if let Some(certificate) = certificate {
            if !certificate.is_string() {
                return Err(OpenClawConfigReadError::Invalid(
                    "Gateway TLS certificate path must be a string".to_string(),
                ));
            }
            config["gateway"]["tls"]["certPath"] = certificate;
        }
        Ok(config)
    })();
    let templated_certificate = local
        .as_ref()
        .ok()
        .and_then(|value| value.pointer("/gateway/tls/certPath"))
        .and_then(Value::as_str)
        .is_some_and(|path| path.contains("${"));
    // Only an unavailable read-authority context may use native resolution or
    // the generic legacy HTTP exception. Invalid/in-bound failures stay errors.
    let native = if matches!(&local, Err(OpenClawConfigReadError::OutsideAuthority(_)))
        || templated_certificate
    {
        match resolve_native() {
            Ok(value) => value,
            Err(error) => {
                *known_tls |= partial_tls;
                return Err(error);
            }
        }
    } else {
        None
    };
    let native_resolved = native.is_some();
    // A successful native lookup completes this initial observation using the
    // whole ordered include graph. Preserve partial evidence only while it is
    // unresolved; never erase a transport chosen during an earlier observation.
    if !native_resolved {
        *known_tls |= partial_tls;
    }
    let mut config = match native {
        Some(gateway) => json!({"gateway": gateway}),
        None => match local {
            Ok(config) => config,
            Err(OpenClawConfigReadError::OutsideAuthority(_)) if !*known_tls => {
                // Known-native bindings return a value or an error, never None.
                // A generic command is not invoked to discover capabilities.
                return Ok(None);
            }
            Err(error) => return Err(error.to_string()),
        },
    };
    let enabled = match config.pointer("/gateway/tls/enabled") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(_) => return Err("Gateway TLS enabled setting must be a boolean".to_string()),
    };
    if !enabled {
        if *known_tls {
            return Err("Gateway TLS was observed during this readiness wait; refusing a plaintext health probe".to_string());
        }
        return Ok(None);
    }
    *known_tls = true;
    if templated_certificate && !native_resolved {
        // Only generic templated bindings need config.env; native bindings own
        // their complete variable-source rules, including dotenv.
        if let Some(values) = read_openclaw_config_field(&paths.config_path, &["env"], &context)
            .value
            .map_err(|error| error.to_string())?
        {
            config["env"] = values;
        }
        config = resolve_openclaw_config_env(
            config,
            env,
            &paths.openclaw_home,
            &paths.state_dir,
            &paths.config_path,
            OpenClawWorkspaceRuntime::for_env(&meta.name, Some(port)),
        );
    }
    let certificate_path = config.pointer("/gateway/tls/certPath");
    if certificate_path.is_some_and(|value| !value.is_string()) {
        return Err("Gateway TLS certificate path must be a string".to_string());
    }
    let raw = certificate_path
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let cert_path = match raw {
        None => paths.state_dir.join("gateway/tls/gateway-cert.pem"),
        Some("~") => paths.openclaw_home.clone(),
        Some(value) if value.starts_with("~/") || value.starts_with("~\\") => {
            let mut expanded = paths.openclaw_home.as_os_str().to_os_string();
            expanded.push(&value[1..]);
            PathBuf::from(expanded)
        }
        Some(value) => run_dir.join(value),
    };
    // Match OpenClaw's bounded public-leaf inspection, including configured symlinks.
    let pem = read_regular_text_file(&cert_path, "Gateway TLS certificate", Some(64 * 1024))
        .map_err(|_| "Gateway TLS public certificate could not be read (missing, unreadable, nonregular, or oversized)".to_string())?;
    let certificate = ureq::tls::Certificate::from_pem(pem.as_bytes())
        .map_err(|_| "Gateway TLS certificate is not a PEM certificate".to_string())?;
    Ok(Some(certificate.der().to_vec()))
}

pub(crate) fn dev_ui_gateway_base_path(
    paths: &EnvPaths,
    context: &OpenClawConfigReadContext,
) -> Result<String, String> {
    let read = |field: &[&str]| {
        read_openclaw_config_field(&paths.config_path, field, context)
            .value
            .map_err(|error| error.to_string())
    };
    if read(&["gateway", "tls", "enabled"])?
        .as_ref()
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Err(
            "dev UI currently requires a local HTTP Gateway; this environment enables Gateway TLS. Use --no-ui to keep using its configured Gateway"
                .to_string(),
        );
    }
    if read(&["gateway", "controlUi", "enabled"])?
        .as_ref()
        .and_then(Value::as_bool)
        == Some(false)
    {
        return Err(
            "the environment disables the Control UI; use --no-ui to keep using its configured Gateway"
                .to_string(),
        );
    }
    match read(&["gateway", "controlUi", "basePath"])? {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err("gateway.controlUi.basePath must be a URL path".to_string()),
    }
}

pub(crate) fn dev_ui_gateway_url(base: &str, port: u32) -> Result<String, String> {
    let base = base.trim().trim_matches('/');
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
    user_home: &Path,
) -> OpenClawConfigAudit {
    let mut issues = Vec::new();
    let foreign_roots = collect_foreign_env_roots(meta, known_envs, value);
    let inferred_roots = collect_inferred_foreign_env_roots(&paths.root, user_home, value);
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

    // A registered root is known to be an environment, so its whole tree moves.
    // An inferred root is only evidenced by its OpenClaw state directory, and
    // only that directory maps onto this environment's state directory.
    let mut repair_rewrites = known_roots
        .iter()
        .map(|root| (root.clone(), paths.root.clone()))
        .collect::<BTreeSet<_>>();
    repair_rewrites.extend(
        inferred_roots
            .keys()
            .filter(|root| !known_roots.contains(*root))
            .map(|root| (root.join(".openclaw"), paths.state_dir.clone())),
    );
    let repair_root_rewrite = if repair_rewrites.len() == 1 {
        repair_rewrites.into_iter().next()
    } else {
        None
    };

    let status = if issues.is_empty() { "ok" } else { "drifted" }.to_string();
    OpenClawConfigAudit {
        status,
        issues,
        repair_root_rewrite,
        repair_workspace,
        repair_gateway_port,
    }
}

fn read_config_value(config_path: &Path) -> Result<Option<Value>, String> {
    read_config_raw(config_path)?
        .as_deref()
        .map(json5::from_str)
        .transpose()
        .map_err(|error| error.to_string())
}

fn read_config_raw(config_path: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(config_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }

    read_regular_text_file(config_path, "OpenClaw config", None).map(Some)
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

fn reject_protected_config_write(state_dir: &Path, path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "copied config has no parent".to_string())?;
    let name = path
        .file_name()
        .ok_or_else(|| "copied config has no filename".to_string())?;
    // Replacement changes parent/name, not an old leaf symlink's referent.
    let destination = fs::canonicalize(parent)
        .map_err(|error| error.to_string())?
        .join(name);
    if super::openclaw_state::is_integrity_protected_history_destination(
        state_dir,
        parent,
        &destination,
    )? {
        return Err(format!(
            "cannot rewrite OpenClaw config in integrity-protected history: {}; use a mutable config include outside the published payload",
            display_path(path)
        ));
    }
    Ok(())
}

fn write_copied_config_value(path: &Path, state_dir: &Path, value: &Value) -> Result<(), String> {
    reject_protected_config_write(state_dir, path)?;
    let permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    let parent = path
        .parent()
        .ok_or_else(|| "copied include has no parent".to_string())?;
    super::openclaw_state::with_owner_write_permission(parent, || {
        #[cfg(unix)]
        {
            // Replace the copied pathname without making a symlink's
            // referent writable. Protect bytes before publishing them.
            let staged = stage_private_config(path, value)?;
            staged
                .as_file()
                .set_permissions(permissions.clone())
                .map_err(|error| error.to_string())?;
            replace_private_config(staged, path)
        }
        #[cfg(not(unix))]
        {
            let write = || write_config_value(path, value);
            if fs::symlink_metadata(path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink()
            {
                write()
            } else {
                super::openclaw_state::with_owner_write_permission(path, write)
            }
        }
    })?;
    #[cfg(not(unix))]
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())?;
    Ok(())
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
    user_home: &Path,
    value: &Value,
) -> BTreeMap<PathBuf, usize> {
    let mut refs = BTreeMap::<PathBuf, usize>::new();
    collect_inferred_foreign_env_roots_inner(current_root, user_home, value, &mut refs);
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
    user_home: &Path,
    value: &Value,
    refs: &mut BTreeMap<PathBuf, usize>,
) {
    match value {
        Value::String(raw) => {
            let path = Path::new(raw);
            let Some(env_root) = inferred_foreign_env_root(current_root, user_home, path) else {
                return;
            };
            *refs.entry(env_root).or_insert(0) += 1;
        }
        Value::Array(values) => {
            for nested in values {
                collect_inferred_foreign_env_roots_inner(current_root, user_home, nested, refs);
            }
        }
        Value::Object(values) => {
            for nested in values.values() {
                collect_inferred_foreign_env_roots_inner(current_root, user_home, nested, refs);
            }
        }
        _ => {}
    }
}

pub(super) fn matching_foreign_env_root(
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

fn inferred_foreign_env_root(
    current_root: &Path,
    user_home: &Path,
    path: &Path,
) -> Option<PathBuf> {
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
            let prefix = clean_path(&prefix);
            // Paths in the user's own OpenClaw home point at live data, not at
            // a copied environment root.
            if prefix == user_home {
                return None;
            }
            return Some(prefix);
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
    rewrite_env_root_paths_from_roots(value, &[source_root], None, target_root)
}

fn rewrite_env_root_paths_from_roots(
    value: &mut Value,
    source_roots: &[&Path],
    canonical_source_root: Option<&Path>,
    target_root: &Path,
) -> bool {
    match value {
        Value::String(raw) => {
            if let [source] = source_roots
                && canonical_source_root.is_none()
            {
                return rewrite_env_root_string(raw, source, target_root);
            }
            if !Path::new(raw).is_absolute() {
                return false;
            }
            // Prefer recorded identities, which also work after source removal.
            // Never run a second pass over a rewritten destination value.
            for source in source_roots {
                if let Ok(suffix) = Path::new(raw).strip_prefix(source) {
                    let Some(target) =
                        map_env_root_suffix(suffix, source, source_roots, target_root)
                    else {
                        return false;
                    };
                    *raw = display_path(&target);
                    return true;
                }
            }
            // Preserve other live aliases of the captured canonical source.
            // Do not re-resolve sourceRoot: its alias may have been retargeted
            // since export, and must not give external paths new ownership.
            if let Some(source) = canonical_source_root
                && let Some(original) = normalize_path_allow_missing(Path::new(raw))
                && let Ok(suffix) = original.strip_prefix(source)
            {
                *raw = display_path(&clean_path(&target_root.join(suffix)));
                return true;
            }
            false
        }
        Value::Array(values) => values.iter_mut().fold(false, |changed, nested| {
            rewrite_env_root_paths_from_roots(
                nested,
                source_roots,
                canonical_source_root,
                target_root,
            ) || changed
        }),
        Value::Object(values) => values.values_mut().fold(false, |changed, nested| {
            rewrite_env_root_paths_from_roots(
                nested,
                source_roots,
                canonical_source_root,
                target_root,
            ) || changed
        }),
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

    let canonical_source = suffix
        .components()
        .any(|component| component == std::path::Component::ParentDir)
        .then(|| normalize_path_allow_missing(source_root))
        .flatten();
    let source_roots = [
        source_root,
        canonical_source.as_deref().unwrap_or(source_root),
    ];
    let Some(target) = map_env_root_suffix(&suffix, source_root, &source_roots, target_root) else {
        return false;
    };
    *raw = display_path(&target);
    true
}

fn map_env_root_suffix(
    suffix: &Path,
    source_root: &Path,
    source_roots: &[&Path],
    target_root: &Path,
) -> Option<PathBuf> {
    if !suffix
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return Some(clean_path(&target_root.join(suffix)));
    }

    // Resolve parent traversal in the recorded source namespace, reading owned
    // entries from the copied tree. Absolute internal links can still name a
    // removed source; neither that source nor a retargeted root alias is read.
    let mut resolved = source_root.to_path_buf();
    let mut pending: VecDeque<OsString> = suffix
        .components()
        .map(|component| component.as_os_str().to_owned())
        .collect();
    let mut links = 0;
    while let Some(component) = pending.pop_front() {
        if component == ".." {
            resolved.pop();
            continue;
        }
        if component == "." {
            continue;
        }
        resolved.push(component);
        let copied = source_roots.iter().find_map(|root| {
            resolved
                .strip_prefix(root)
                .ok()
                .map(|relative| target_root.join(relative))
        });
        let entry = copied.as_deref().unwrap_or(&resolved);
        match fs::symlink_metadata(entry) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                links += 1;
                if links > 40 {
                    return None;
                }
                let link = fs::read_link(entry).ok()?;
                resolved.pop();
                let remainder = if link.is_absolute() {
                    if let Some((root, relative)) = source_roots
                        .iter()
                        .find_map(|root| link.strip_prefix(root).ok().map(|path| (*root, path)))
                    {
                        resolved = root.to_path_buf();
                        relative
                    } else if let Ok(relative) = link.strip_prefix(target_root) {
                        resolved = source_root.to_path_buf();
                        relative
                    } else {
                        // External links retain normal filesystem semantics.
                        // A final external destination must not be relocated.
                        let rest: PathBuf = pending.iter().collect();
                        let external = normalize_path_allow_missing(&link.join(rest))?;
                        return source_roots.iter().find_map(|root| {
                            external
                                .strip_prefix(root)
                                .ok()
                                .map(|relative| target_root.join(relative))
                        });
                    }
                } else {
                    &link
                };
                for component in remainder.components().rev() {
                    pending.push_front(component.as_os_str().to_owned());
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    source_roots.iter().find_map(|root| {
        resolved
            .strip_prefix(root)
            .ok()
            .map(|relative| target_root.join(relative))
    })
}

pub(super) fn normalize_path_allow_missing(path: &Path) -> Option<PathBuf> {
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

    #[cfg(unix)]
    #[test]
    fn migration_home_expansion_follows_openclaw_tilde_rules() {
        let home = Path::new("/home/alice");
        let expand = |raw| expand_home_relative_path(raw, home, true);
        assert_eq!(expand("~/dev/p").as_deref(), Some("/home/alice/dev/p"));
        assert_eq!(expand(" ~/dev/p ").as_deref(), Some("/home/alice/dev/p"));
        // Trimmed as JavaScript trims: BOM yes, U+0085 no.
        assert_eq!(
            expand("\u{feff}~/dev/p").as_deref(),
            Some("/home/alice/dev/p")
        );
        assert_eq!(expand("\u{85}~/ready"), None);
        assert_eq!(expand("~").as_deref(), Some("/home/alice"));
        assert_eq!(
            expand("~/a/./../dev/p").as_deref(),
            Some("/home/alice/dev/p")
        );
        assert_eq!(
            expand("~/.openclaw/../dev/p").as_deref(),
            Some("/home/alice/dev/p")
        );
        assert_eq!(
            expand("~/.openclawish/p").as_deref(),
            Some("/home/alice/.openclawish/p")
        );
        // Values under `~/.openclaw` already name the environment's copy.
        assert_eq!(expand("~/.openclaw/extensions/x"), None);
        assert_eq!(expand("~/x/../.openclaw/y"), None);
        // OpenClaw replaces the `~` of `~\` too; on POSIX the rest stays one name.
        assert_eq!(expand("~\\dev").as_deref(), Some("/home/alice\\dev"));
        for unchanged in ["~alice/dev", "dev/~/p", "/abs/~/p", "~~/p"] {
            assert_eq!(expand(unchanged), None, "{unchanged}");
        }
        // OpenClaw substitutes a placeholder before it resolves `..`, so only
        // the literal directories before it are resolved here.
        assert_eq!(
            expand("~/${UP}/../shared").as_deref(),
            Some("/home/alice/${UP}/../shared")
        );
        assert_eq!(
            expand("~/a/../dev/${SUB}/x").as_deref(),
            Some("/home/alice/dev/${SUB}/x")
        );
        assert_eq!(
            expand("~/dev/p${N}.log").as_deref(),
            Some("/home/alice/dev/p${N}.log")
        );
        assert_eq!(expand("~/../../${X}").as_deref(), Some("/${X}"));
        // A value that resolves to the filesystem root has nothing after it.
        assert_eq!(expand("~/../..").as_deref(), Some("/"));
        assert_eq!(
            expand_home_relative_path("~", Path::new("/"), true).as_deref(),
            Some("/")
        );
        // On POSIX `\` is part of the name, which stays after a root home.
        assert_eq!(
            expand_home_relative_path("~\\${X}", Path::new("/"), true).as_deref(),
            Some("/\\${X}")
        );
        assert_eq!(expand("~/.openclaw/${SUB}/x"), None);
        assert_eq!(
            expand("~/.openclaw/../${SUB}").as_deref(),
            Some("/home/alice/${SUB}")
        );
        // A setting that does not trim keeps its whitespace semantics.
        assert_eq!(expand_home_relative_path(" ~/s.json", home, false), None);
        assert_eq!(
            expand_home_relative_path("~/s.json ", home, false).as_deref(),
            Some("/home/alice/s.json ")
        );
    }

    #[test]
    fn migration_home_expansion_selects_the_settings_openclaw_expands() {
        let setting = |scope: &[&str]| {
            let scope = scope
                .iter()
                .map(|segment| match *segment {
                    "[]" => ConfigSegment::Index(0),
                    key => ConfigSegment::Key(key.to_string()),
                })
                .collect::<Vec<_>>();
            home_relative_path_setting(&scope)
        };
        for path in [
            &["plugins", "load", "paths", "[]"][..],
            &["plugins", "load", "paths"],
            &["tools", "exec", "pathPrepend", "[]"],
            &["plugins", "entries", "demo", "config", "pathPrepend"],
            &["skills", "load", "extraDirs", "[]"],
            &["hooks", "internal", "load", "extraDirs", "[]"],
            &["agents", "list", "[]", "agentDir"],
            &["agents", "defaults", "cwd"],
            &["agents", "entries", "main", "cwd"],
            &["agents", "list", "[]", "cwd"],
            &["gateway", "tls", "caPath"],
            &["logging", "file"],
            &["plugins", "entries", "demo", "config", "rootDir"],
            &["agents", "defaults", "workspace"],
            &["cron", "store"],
        ] {
            assert_eq!(
                setting(path),
                Some(HomePathRule::Lexical { trims: true }),
                "{path:?}"
            );
        }
        assert_eq!(
            setting(&["session", "store"]),
            Some(HomePathRule::Lexical { trims: false })
        );
        assert_eq!(
            setting(&["nodeHost", "workerRuns", "folders", "[]"]),
            Some(HomePathRule::Physical)
        );
        assert_eq!(
            setting(&["skills", "load", "allowSymlinkTargets", "[]"]),
            Some(HomePathRule::TrustRoot)
        );
        assert_eq!(
            setting(&["security", "installPolicy", "exec", "trustedDirs", "[]"]),
            Some(HomePathRule::TrustRoot)
        );
        for text in [
            &["messages", "responsePrefix"][..],
            &["messages", "usageTemplate"],
            // Expanded against the OS home, which the environment keeps.
            &["memory", "search", "extraPaths", "[]"],
            &["plugins", "entries", "voice-call", "config", "store"],
            &[
                "plugins",
                "entries",
                "file-transfer",
                "config",
                "nodes",
                "n1",
                "allowReadPaths",
                "[]",
            ],
            &["agents", "list", "[]", "identity", "name"],
            &["plugins", "entries", "demo", "config", "dirs", "[]"],
            // A digit-only key is an object key, not a list entry.
            &["plugins", "entries", "demo", "config", "paths", "0"],
            &["memory", "store"],
            &["agents", "defaults", "cwd", "[]"],
            &["nodeHost", "workerRuns", "folders"],
            &[],
        ] {
            assert_eq!(setting(text), None, "{text:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn migration_home_expansion_keeps_link_traversal_for_physical_settings() {
        let expansion = HomeExpansion {
            home: Path::new("/home/alice"),
            source_state_dir: Path::new("/home/alice/.openclaw"),
            target_state_dir: Path::new("/envs/copied/.openclaw"),
        };
        let expand = |raw| expand_physical_home_relative_path(raw, &expansion);
        assert_eq!(expand("~/work").as_deref(), Some("/home/alice/work"));
        assert_eq!(expand("~").as_deref(), Some("/home/alice"));
        // `..` stays for the consumer to resolve after following `hop`.
        assert_eq!(
            expand("~/hop/../work").as_deref(),
            Some("/home/alice/hop/../work")
        );
        // `..` that leaves the state directory is not moved into the copy.
        assert_eq!(
            expand("~/.openclaw/../work").as_deref(),
            Some("/home/alice/.openclaw/../work")
        );
        assert_eq!(
            expand("~/.openclaw/../../shared").as_deref(),
            Some("/home/alice/.openclaw/../../shared")
        );
        // The relocation would step over `missing`, which does not resolve.
        assert_eq!(expand("~/.openclaw/missing/../wf"), None);
        assert_eq!(
            expand("~/${SUB}/work").as_deref(),
            Some("/home/alice/${SUB}/work")
        );
        // The consumer does not trim.
        assert_eq!(expand(" ~/work"), None);
        for copied in [
            "~/.openclaw",
            "~/.openclaw/wf",
            "~/.openclaw/${SUB}",
            "~/.openclaw/a/../${SUB}",
        ] {
            assert_eq!(expand(copied), None, "{copied}");
        }
        assert_eq!(
            expand("~/.openclawish").as_deref(),
            Some("/home/alice/.openclawish")
        );
    }

    #[test]
    fn migration_home_expansion_types_include_scopes_from_traversal() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("openclaw.json");
        // Concatenating `a.json` and `b.json` merges an object with a
        // digit-only key and a list into one `paths` value; the include at
        // the list entry keeps its list-index segment.
        fs::write(&config, r#"{"hooks":{"$include":["./a.json","./b.json"]}}"#).unwrap();
        fs::write(
            root.path().join("a.json"),
            r#"{"items":[{"paths":{"0":"literal"}}]}"#,
        )
        .unwrap();
        fs::write(
            root.path().join("b.json"),
            r#"{"items":[{"paths":[{"$include":"./p.json"}]}]}"#,
        )
        .unwrap();
        fs::write(root.path().join("p.json"), r#""~/bin""#).unwrap();
        let effective = load_effective_openclaw_config(&config).unwrap().unwrap();
        let (_, scopes) = effective
            .include_scopes
            .iter()
            .find(|(path, _)| path.ends_with("p.json"))
            .unwrap();
        assert_eq!(
            scopes.iter().collect::<Vec<_>>(),
            [&vec![
                ConfigSegment::Key("hooks".to_string()),
                ConfigSegment::Key("items".to_string()),
                ConfigSegment::Index(0),
                ConfigSegment::Key("paths".to_string()),
                ConfigSegment::Index(0),
            ]]
        );
        assert_eq!(
            home_relative_path_setting(scopes.first().unwrap()),
            Some(HomePathRule::Lexical { trims: true })
        );
        assert_eq!(
            config_setting_name(scopes.first().unwrap()),
            "hooks.items[].paths[]"
        );
    }

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
    fn readiness_tailnet_bind_uses_launch_scoped_include_and_variables() {
        let root = tempfile::tempdir().unwrap();
        let paths = derive_env_paths(&root.path().join("environment"));
        fs::create_dir_all(&paths.state_dir).unwrap();
        let meta = meta("demo", paths.root.to_str().unwrap(), Some(18789));
        fs::write(&paths.config_path, r#"{"$include":"bind.json"}"#).unwrap();
        fs::write(
            paths.state_dir.join("bind.json"),
            r#"{"gateway":{"bind":"${BIND_MODE}"}}"#,
        )
        .unwrap();
        let env = BTreeMap::from([("BIND_MODE".to_string(), "tailnet".to_string())]);
        assert_eq!(
            gateway_readiness_host(&meta, 18789, root.path(), &env, || Ok(None)).unwrap(),
            GatewayReadinessHost::Tailnet
        );
        assert_eq!(
            gateway_readiness_host(&meta, 18789, root.path(), &env, || Ok(Some(
                json!({"bind":"custom", "customBindHost":"127.0.0.2"})
            )))
            .unwrap(),
            GatewayReadinessHost::Address("127.0.0.2".parse().unwrap())
        );
    }

    #[test]
    fn readiness_certificate_uses_isolated_home_and_fails_closed_on_missing_or_invalid_leaf() {
        let root = tempfile::tempdir().unwrap();
        let paths = derive_env_paths(&root.path().join("environment"));
        fs::create_dir_all(&paths.state_dir).unwrap();
        let meta = meta("demo", paths.root.to_str().unwrap(), Some(18789));
        let cert_path = paths.openclaw_home.join("cert.pem");
        // This tests path/PEM resolution only; live TLS tests prove certificate
        // and handshake validity with a real synthetic certificate/key pair.
        let pem = "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";
        fs::write(&cert_path, pem).unwrap();
        let config = json!({"gateway":{"tls":{"enabled":true,"certPath":" ~/cert.pem "}}});
        let original = serde_json::to_vec(&config).unwrap();
        fs::write(&paths.config_path, &original).unwrap();
        let env = BTreeMap::from([(
            "HOME".to_string(),
            display_path(&root.path().join("wrong-home")),
        )]);
        let mut known_tls = false;
        assert_eq!(
            gateway_readiness_certificate(&meta, 18789, root.path(), &env, &mut known_tls, || Ok(
                None
            ))
            .unwrap(),
            Some(vec![1, 2, 3])
        );
        fs::remove_file(&cert_path).unwrap();
        assert!(
            gateway_readiness_certificate(&meta, 18789, root.path(), &env, &mut known_tls, || Ok(
                None
            ))
            .is_err()
        );
        fs::write(&cert_path, "invalid public certificate").unwrap();
        assert!(
            gateway_readiness_certificate(&meta, 18789, root.path(), &env, &mut known_tls, || Ok(
                None
            ))
            .is_err()
        );
        fs::write(&cert_path, pem).unwrap();
        assert!(
            gateway_readiness_certificate(&meta, 18789, root.path(), &env, &mut known_tls, || Ok(
                None
            ))
            .unwrap()
            .is_some()
        );
        assert_eq!(fs::read(&paths.config_path).unwrap(), original);
        assert!(!paths.state_dir.join("gateway/tls/gateway-key.pem").exists());
    }

    #[test]
    fn gateway_url_for_launch_uses_the_readiness_tls_decision() {
        let root = tempfile::tempdir().unwrap();
        let paths = derive_env_paths(&root.path().join("environment"));
        fs::create_dir_all(&paths.state_dir).unwrap();
        let meta = meta("demo", paths.root.to_str().unwrap(), Some(18789));
        let env = BTreeMap::new();
        let url = |config: Value| {
            fs::write(&paths.config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            gateway_url_for_launch(&meta, 18789, root.path(), &env)
        };
        assert_eq!(url(json!({"gateway":{}})), "http://127.0.0.1:18789");
        assert_eq!(
            url(json!({"gateway":{"tls":{"enabled":false}}})),
            "http://127.0.0.1:18789"
        );
        // The transport does not depend on a readable certificate or on a
        // native lookup of a templated certificate path.
        assert_eq!(
            url(json!({"gateway":{"tls":{"enabled":true}}})),
            "https://127.0.0.1:18789"
        );
        assert_eq!(
            url(json!({"gateway":{"tls":{"enabled":true,"certPath":"${UNSET_CERT}"}}})),
            "https://127.0.0.1:18789"
        );
        // TLS observed before an unreadable include is still TLS evidence.
        let known = paths.config_path.with_file_name("known-tls.json");
        fs::write(&known, r#"{"gateway":{"tls":{"enabled":true}}}"#).unwrap();
        let outside = root.path().join("shared.json");
        fs::write(&outside, "{}").unwrap();
        assert_eq!(
            url(json!({"$include": [known, outside]})),
            "https://127.0.0.1:18789"
        );
        // An invalid setting is not TLS evidence.
        assert_eq!(
            url(json!({"gateway":{"tls":{"enabled":"yes"}}})),
            "http://127.0.0.1:18789"
        );
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
    fn minimum_config_preserves_nested_includes_and_fills_only_absent_defaults() {
        for authored in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let paths = derive_env_paths(root.path());
            fs::create_dir_all(&paths.state_dir).unwrap();
            let gateway = if authored {
                json!({"mode": "remote", "bind": "lan", "port": 19800})
            } else {
                json!({"port": 19800})
            };
            let defaults = if authored {
                json!({"workspace": display_path(&paths.state_dir.join("custom"))})
            } else {
                json!({"model": "authored-model"})
            };
            let gateway_path = paths.state_dir.join("gateway.json");
            let defaults_path = paths.state_dir.join("defaults.json");
            let gateway_bytes = serde_json::to_vec(&gateway).unwrap();
            let defaults_bytes = serde_json::to_vec(&defaults).unwrap();
            fs::write(&gateway_path, &gateway_bytes).unwrap();
            fs::write(&defaults_path, &defaults_bytes).unwrap();
            write_config_value(
                &paths.config_path,
                &json!({
                    "gateway": {"$include": "./gateway.json"},
                    "agents": {"defaults": {"$include": "./defaults.json"}}
                }),
            )
            .unwrap();

            ensure_minimum_local_openclaw_config(&paths, 19801).unwrap();
            let effective = load_effective_openclaw_config(&paths.config_path)
                .unwrap()
                .unwrap()
                .value;
            assert_eq!(effective["gateway"]["port"], 19801);
            assert_eq!(
                effective["gateway"]["mode"],
                if authored { "remote" } else { "local" }
            );
            assert_eq!(
                effective["gateway"]["bind"],
                if authored { "lan" } else { "loopback" }
            );
            assert_eq!(
                effective["agents"]["defaults"]["workspace"],
                if authored {
                    display_path(&paths.state_dir.join("custom"))
                } else {
                    display_path(&paths.workspace_dir)
                }
            );
            let raw = read_config_value(&paths.config_path).unwrap().unwrap();
            assert_eq!(raw["gateway"]["$include"], "./gateway.json");
            assert_eq!(raw["agents"]["defaults"]["$include"], "./defaults.json");
            if authored {
                assert!(raw["gateway"].get("mode").is_none());
                assert!(raw["gateway"].get("bind").is_none());
                assert!(raw["agents"]["defaults"].get("workspace").is_none());
            } else {
                assert_eq!(effective["agents"]["defaults"]["model"], "authored-model");
            }
            assert_eq!(fs::read(&gateway_path).unwrap(), gateway_bytes);
            assert_eq!(fs::read(&defaults_path).unwrap(), defaults_bytes);
            let once = fs::read(&paths.config_path).unwrap();
            ensure_minimum_local_openclaw_config(&paths, 19801).unwrap();
            assert_eq!(fs::read(&paths.config_path).unwrap(), once);
        }
    }

    #[test]
    fn minimum_config_does_not_write_when_include_resolution_fails() {
        let root = tempfile::tempdir().unwrap();
        let paths = derive_env_paths(root.path());
        fs::create_dir_all(&paths.state_dir).unwrap();
        let original = b"{ $include: './missing.json' }\n";
        fs::write(&paths.config_path, original).unwrap();
        assert!(ensure_minimum_local_openclaw_config(&paths, 19800).is_err());
        assert_eq!(fs::read(&paths.config_path).unwrap(), original);
    }

    #[test]
    fn prepared_minimum_config_does_not_overwrite_a_changed_root() {
        let root = tempfile::tempdir().unwrap();
        let paths = derive_env_paths(root.path());
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(&paths.config_path, "{}").unwrap();
        let plan = prepare_minimum_local_openclaw_config(
            &paths,
            19800,
            &OpenClawConfigReadContext::default(),
        )
        .unwrap();
        let changed = "// changed during binding preparation\n{gateway:{bind:'lan'}}";
        fs::write(&paths.config_path, changed).unwrap();
        assert!(
            plan.apply()
                .unwrap_err()
                .contains("changed during minimum setup")
        );
        assert_eq!(fs::read_to_string(&paths.config_path).unwrap(), changed);
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
            Path::new("/home/user"),
            Path::new("/tmp/ocm/envs/source/.openclaw/workspace/notes.txt"),
        );
        assert_eq!(inferred, Some(PathBuf::from("/tmp/ocm/envs/source")));
    }

    #[test]
    fn inferred_foreign_env_root_ignores_the_users_openclaw_home() {
        let inferred = inferred_foreign_env_root(
            Path::new("/home/user/.ocm/envs/target"),
            Path::new("/home/user"),
            Path::new("/home/user/.openclaw/skills"),
        );
        assert_eq!(inferred, None);
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

        let audit =
            audit_openclaw_config_value(&current, &[], &paths, &value, Path::new("/home/user"));
        assert_eq!(audit.status, "drifted");
        assert_eq!(
            audit.repair_root_rewrite,
            Some((
                PathBuf::from("/tmp/ocm/envs/source/.openclaw"),
                PathBuf::from("/tmp/ocm/envs/target/.openclaw")
            ))
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

        let audit =
            audit_openclaw_config_value(&current, &[], &paths, &value, Path::new("/home/user"));

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
