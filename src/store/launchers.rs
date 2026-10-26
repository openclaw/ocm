use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::launcher::{AddLauncherOptions, LauncherMeta};

use super::common::{load_json_files, path_exists, read_json, read_json_if_present, write_json};
use super::envs::with_locked_environments;
use super::layout::{display_path, launcher_meta_path, resolve_absolute_path, validate_name};
use super::now_utc;

pub fn list_launchers(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Vec<LauncherMeta>, String> {
    let stores = super::ensure_store(env, cwd)?;
    let files = load_json_files(&stores.launchers_dir)?;
    let mut out: Vec<LauncherMeta> = Vec::with_capacity(files.len());
    for file in files {
        out.push(read_json(&file)?);
    }
    out.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(out)
}

pub fn get_launcher(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<LauncherMeta, String> {
    let safe_name = validate_name(name, "Launcher name")?;
    find_launcher(&safe_name, env, cwd)?
        .ok_or_else(|| format!("launcher \"{safe_name}\" does not exist"))
}

pub(crate) fn find_launcher(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Option<LauncherMeta>, String> {
    let safe_name = validate_name(name, "Launcher name")?;
    let path = launcher_meta_path(&safe_name, env, cwd)?;
    read_json_if_present(&path)
}

pub fn add_launcher(
    options: AddLauncherOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<LauncherMeta, String> {
    let name = validate_name(&options.name, "Launcher name")?;
    let meta_path = launcher_meta_path(&name, env, cwd)?;
    if path_exists(&meta_path) {
        return Err(format!("launcher \"{name}\" already exists"));
    }

    let command = options.command.trim();
    if command.is_empty() {
        return Err("launcher command is required".to_string());
    }

    let launcher_cwd = match options.cwd.as_deref() {
        Some(raw) if !raw.trim().is_empty() => {
            Some(display_path(&resolve_absolute_path(raw, env, cwd)?))
        }
        _ => None,
    };
    let description = options
        .description
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let created_at = now_utc();
    let meta = LauncherMeta {
        kind: "ocm-launcher".to_string(),
        name,
        command: command.to_string(),
        cwd: launcher_cwd,
        description,
        created_at,
        updated_at: created_at,
    };
    write_json(&meta_path, &meta)?;
    Ok(meta)
}

pub fn remove_launcher(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<LauncherMeta, String> {
    let meta = get_launcher(name, env, cwd)?;
    with_locked_environments(env, cwd, |envs| {
        let bound_envs = envs
            .iter()
            .filter_map(|environment| {
                let launcher_name = environment.default_launcher.as_deref()?;
                let launcher = get_launcher(launcher_name, env, cwd).ok()?;
                (launcher.name == meta.name).then_some(environment.name.as_str())
            })
            .collect::<Vec<_>>();
        if !bound_envs.is_empty() {
            return Err(format!(
                "launcher \"{}\" is still used by environment(s): {}; clear those bindings before removing it",
                meta.name,
                bound_envs.join(", ")
            ));
        }
        let path = launcher_meta_path(&meta.name, env, cwd)?;
        fs::remove_file(path).map_err(|error| error.to_string())
    })?;
    Ok(meta)
}
