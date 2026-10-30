use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub struct EnvPaths {
    pub root: PathBuf,
    pub openclaw_home: PathBuf,
    pub state_dir: PathBuf,
    pub config_path: PathBuf,
    pub workspace_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct StorePaths {
    pub home: PathBuf,
    pub envs_dir: PathBuf,
    pub launchers_dir: PathBuf,
    pub runtimes_dir: PathBuf,
    pub snapshots_dir: PathBuf,
    pub upgrade_history_dir: PathBuf,
    pub supervisor_dir: PathBuf,
}

pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn clean_path(path: &Path) -> PathBuf {
    let mut parts: Vec<OsString> = Vec::new();
    let mut prefix: Option<OsString> = None;
    let mut absolute = false;

    for component in path.components() {
        match component {
            Component::Prefix(value) => prefix = Some(value.as_os_str().to_os_string()),
            Component::RootDir => absolute = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(last) = parts.last() {
                    if last != OsStr::new("..") {
                        parts.pop();
                    } else if !absolute {
                        parts.push(OsString::from(".."));
                    }
                } else if !absolute {
                    parts.push(OsString::from(".."));
                }
            }
            Component::Normal(value) => parts.push(value.to_os_string()),
        }
    }

    let mut out = PathBuf::new();
    if let Some(prefix) = prefix {
        out.push(prefix);
    }
    if absolute {
        out.push(Path::new(std::path::MAIN_SEPARATOR_STR));
    }
    for part in parts {
        out.push(part);
    }

    if out.as_os_str().is_empty() {
        if absolute {
            out.push(Path::new(std::path::MAIN_SEPARATOR_STR));
        } else {
            out.push(".");
        }
    }

    out
}

fn normalize_value(value: &str) -> &str {
    value.trim()
}

pub fn resolve_user_home(env: &BTreeMap<String, String>) -> PathBuf {
    if let Some(home) = env.get("HOME").map(String::as_str).map(normalize_value)
        && !home.is_empty()
    {
        return PathBuf::from(home);
    }

    if let Some(home) = env
        .get("USERPROFILE")
        .map(String::as_str)
        .map(normalize_value)
        && !home.is_empty()
    {
        return PathBuf::from(home);
    }

    std::env::var("HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn resolve_absolute_path(
    input: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let raw = normalize_value(input);
    if raw.is_empty() {
        return Err("path is required".to_string());
    }

    let path = match raw {
        "~" => resolve_user_home(env),
        _ if raw.starts_with("~/") || raw.starts_with("~\\") => {
            resolve_user_home(env).join(&raw[2..])
        }
        _ => {
            let path = Path::new(raw);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            }
        }
    };

    Ok(clean_path(&path))
}

pub fn resolve_ocm_home(env: &BTreeMap<String, String>, cwd: &Path) -> Result<PathBuf, String> {
    if let Some(override_value) = env.get("OCM_HOME") {
        let trimmed = normalize_value(override_value);
        if !trimmed.is_empty() {
            return resolve_absolute_path(trimmed, env, cwd);
        }
    }

    Ok(clean_path(&resolve_user_home(env).join(".ocm")))
}

pub fn resolve_store_paths(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<StorePaths, String> {
    let home = resolve_ocm_home(env, cwd)?;
    Ok(StorePaths {
        envs_dir: home.join("envs"),
        launchers_dir: home.join("launchers"),
        runtimes_dir: home.join("runtimes"),
        snapshots_dir: home.join("snapshots"),
        upgrade_history_dir: home.join("upgrade-history"),
        supervisor_dir: home.join("supervisor"),
        home,
    })
}

pub fn validate_name(name: &str, label: &str) -> Result<String, String> {
    let trimmed = normalize_value(name);
    if trimmed.is_empty() {
        return Err(format!("{label} is required"));
    }

    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return Err(format!("{label} is required"));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(format!(
            "{label} must use letters, numbers, '.', '_', or '-'"
        ));
    }
    if chars.any(|ch| !ch.is_ascii_alphanumeric() && ch != '.' && ch != '_' && ch != '-') {
        return Err(format!(
            "{label} must use letters, numbers, '.', '_', or '-'"
        ));
    }

    Ok(trimmed.to_string())
}

/// Validates a new runtime or launcher name. `env set-runtime` and
/// `env set-launcher` read `none` in any letter case as "clear the binding",
/// so an entry with that name could not be bound by name.
pub(crate) fn validate_registration_name(name: &str, label: &str) -> Result<String, String> {
    let name = validate_name(name, label)?;
    if name.eq_ignore_ascii_case("none") {
        return Err(format!(
            "{label} \"{name}\" is reserved; `none` clears a binding in env set-runtime and env set-launcher"
        ));
    }
    Ok(name)
}

pub fn derive_env_paths(root: impl AsRef<Path>) -> EnvPaths {
    let clean_root = clean_path(root.as_ref());
    let state_dir = clean_root.join(".openclaw");
    EnvPaths {
        root: clean_root.clone(),
        openclaw_home: clean_root.clone(),
        state_dir: state_dir.clone(),
        config_path: state_dir.join("openclaw.json"),
        workspace_dir: state_dir.join("workspace"),
    }
}

pub fn default_env_root(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores.envs_dir.join(name))
}

pub fn env_registry_path(env: &BTreeMap<String, String>, cwd: &Path) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores.home.join("envs.json"))
}

pub fn launcher_meta_path(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores.launchers_dir.join(format!("{name}.json")))
}

pub fn runtime_meta_path(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores.runtimes_dir.join(format!("{name}.json")))
}

pub fn runtime_install_root(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores.runtimes_dir.join(name))
}

pub fn runtime_install_files_dir(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    Ok(runtime_install_root(name, env, cwd)?.join("files"))
}

pub fn snapshot_env_dir(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores.snapshots_dir.join(env_name))
}

pub fn snapshot_meta_path(
    env_name: &str,
    snapshot_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    Ok(snapshot_env_dir(env_name, env, cwd)?.join(format!("{snapshot_id}.json")))
}

pub fn upgrade_history_env_dir(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores.upgrade_history_dir.join(env_name))
}

pub fn upgrade_history_meta_path(
    env_name: &str,
    transaction_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    Ok(upgrade_history_env_dir(env_name, env, cwd)?.join(format!("{transaction_id}.json")))
}

pub fn upgrade_history_recovery_dir(
    env_name: &str,
    transaction_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    Ok(upgrade_history_env_dir(env_name, env, cwd)?.join(format!("{transaction_id}.recovery")))
}

pub fn upgrade_history_runtime_recovery_dir(
    env_name: &str,
    transaction_id: &str,
    runtime_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    Ok(upgrade_history_recovery_dir(env_name, transaction_id, env, cwd)?.join(runtime_name))
}

pub fn supervisor_state_path(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores.supervisor_dir.join("state.json"))
}

pub fn supervisor_runtime_path(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores.supervisor_dir.join("runtime.json"))
}

pub fn supervisor_logs_dir(env: &BTreeMap<String, String>, cwd: &Path) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores.supervisor_dir.join("logs"))
}

/// The file where the supervisor captures one environment's service output.
/// It lives outside the environment root and is keyed only by name, so it is
/// discarded when the name is registered for a new environment.
pub fn supervisor_child_log_path(logs_dir: &Path, env_name: &str, stream: &str) -> PathBuf {
    let prefix = if env_name.eq_ignore_ascii_case("daemon") {
        "."
    } else {
        ""
    };
    logs_dir.join(format!("{prefix}{env_name}.{stream}.log"))
}

/// A case-distinct file name for an exact environment name. The SHA-256 of the
/// exact name keeps every accepted name distinct under case folding and keeps
/// the file name short for any accepted name length.
fn case_distinct_key(name: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(name.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Separate service output for a name whose name-keyed file already belongs to
/// a registered case-distinct environment on case-insensitive storage.
pub(crate) fn supervisor_child_case_distinct_log_path(
    logs_dir: &Path,
    env_name: &str,
    stream: &str,
) -> PathBuf {
    logs_dir
        .join("case-distinct")
        .join(format!("{}.{stream}.log", case_distinct_key(env_name)))
}

/// The file that holds one environment's service output. On case-insensitive
/// storage `alpha.stdout.log` can open `Alpha.stdout.log`. The directory entry
/// keeps the spelling of the name that created it. While that name is a
/// registered environment, it keeps the file and the other name uses its own
/// case-distinct file, and a name keeps its case-distinct file once it exists.
/// Every other name-keyed file, including one left by a removed environment, is
/// used unchanged.
pub(crate) fn supervisor_child_log_file(
    path: &Path,
    env_name: &str,
    stream: &str,
    is_registered: impl FnOnce(&str) -> bool,
) -> PathBuf {
    resolve_child_log_file(path, env_name, stream, is_registered).0
}

/// The file a newly spawned child writes. A file left under a removed
/// environment's spelling is renamed to this name's spelling first, so the
/// file stays this environment's when that spelling is registered again.
pub(crate) fn adopt_supervisor_child_log_file(
    path: &Path,
    env_name: &str,
    stream: &str,
    is_registered: impl FnOnce(&str) -> bool,
) -> PathBuf {
    let (file, leftover) = resolve_child_log_file(path, env_name, stream, is_registered);
    if let Some(leftover) = leftover {
        // A failed rename leaves the spelling as it was; output still goes to
        // the same file.
        let _ = fs::rename(leftover, &file);
    }
    file
}

/// The resolved file, and the entry it opens when that entry is spelled for a
/// name that is not registered.
fn resolve_child_log_file(
    path: &Path,
    env_name: &str,
    stream: &str,
    is_registered: impl FnOnce(&str) -> bool,
) -> (PathBuf, Option<PathBuf>) {
    let unchanged = || (path.to_path_buf(), None);
    let (Some(dir), Some(file_name)) = (path.parent(), path.file_name()) else {
        return unchanged();
    };
    let own_name = format!("{env_name}.{stream}.log");
    let Some(own) = file_name
        .to_str()
        .filter(|name| *name == own_name || name.strip_prefix('.') == Some(own_name.as_str()))
    else {
        return unchanged();
    };
    let separate = supervisor_child_case_distinct_log_path(dir, env_name, stream);
    if fs::symlink_metadata(&separate).is_ok() {
        return (separate, None);
    }
    if fs::symlink_metadata(path).is_err() {
        return unchanged();
    }
    // A listing that cannot be read completely proves nothing about the
    // spelling; keep the name-keyed file rather than hiding existing output.
    let Ok(entries) = fs::read_dir(dir).and_then(|entries| entries.collect::<Result<Vec<_>, _>>())
    else {
        return unchanged();
    };
    let names = entries
        .iter()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    if names.iter().any(|name| name == own) {
        return unchanged();
    }
    let prefix = &own[..own.len() - own_name.len()];
    let suffix = format!(".{stream}.log");
    let Some((entry, creator)) = names.iter().find_map(|name| {
        let creator = name.strip_prefix(prefix)?.strip_suffix(&suffix)?;
        name.eq_ignore_ascii_case(own).then_some((name, creator))
    }) else {
        return unchanged();
    };
    if creator == env_name {
        unchanged()
    } else if is_registered(creator) {
        (separate, None)
    } else {
        (path.to_path_buf(), Some(dir.join(entry)))
    }
}

/// Old plans can name the shared daemon's diagnostic file. Keep the saved
/// launch identity intact, but separate the output of each newly spawned child.
/// An explicit destination outside that reserved basename remains unchanged.
pub(crate) fn supervisor_child_log_destination(
    path: &Path,
    env_name: &str,
    stream: &str,
) -> PathBuf {
    if env_name.eq_ignore_ascii_case("daemon")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.eq_ignore_ascii_case("daemon.stdout.log")
                    || name.eq_ignore_ascii_case("daemon.stderr.log")
            })
    {
        path.with_file_name(format!(".{env_name}.{stream}.log"))
    } else {
        path.to_path_buf()
    }
}

pub fn source_watch_override_path(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores
        .home
        .join("source-watch")
        .join(format!("{name}.json")))
}

/// Separate source-watch state for a name whose `source-watch/<name>.*` files
/// already belong to a case-distinct environment on case-insensitive storage.
/// The SHA-256 of the exact name keeps every accepted name distinct under case
/// folding and keeps the file name short for any accepted name length.
pub fn source_watch_case_distinct_override_path(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores
        .home
        .join("source-watch")
        .join("case-distinct")
        .join(format!("{}.json", case_distinct_key(name))))
}

pub fn snapshot_archive_path(
    env_name: &str,
    snapshot_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    Ok(snapshot_env_dir(env_name, env, cwd)?.join(format!("{snapshot_id}.tar")))
}

pub fn snapshot_checkpoint_path(
    env_name: &str,
    snapshot_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    Ok(snapshot_env_dir(env_name, env, cwd)?.join(format!("{snapshot_id}.checkpoint")))
}
