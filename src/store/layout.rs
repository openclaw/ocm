use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
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
