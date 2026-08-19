use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use crate::host::verify_official_openclaw_runtime_host;
use crate::infra::download::{
    artifact_file_name_from_url, download_to_file, file_sha256, normalize_file_integrity,
    normalize_sha256, verify_file_integrity, verify_file_sha256,
};
use crate::infra::tree_digest::tree_sha256;
use crate::managed_node::{CommandSpec, managed_runtime_install_command};
use crate::runtime::releases::{
    OpenClawRelease, RuntimeRelease, load_official_openclaw_release_selection,
    load_release_manifest, normalize_openclaw_channel_selector, official_openclaw_releases_url,
    select_official_openclaw_release_by_channel, select_official_openclaw_release_by_version,
    select_release,
};
use crate::runtime::{
    AddRuntimeOptions, InstallRuntimeFromOfficialReleaseOptions, InstallRuntimeFromReleaseOptions,
    InstallRuntimeFromUrlOptions, InstallRuntimeOptions, RuntimeMeta, RuntimeReleaseSelectorKind,
    RuntimeSourceKind, is_official_openclaw_package_runtime, is_openclaw_package_runtime,
};

use super::common::{
    ExclusiveFileLock, copy_dir_recursive, ensure_dir, load_json_files, lock_file, path_exists,
    read_json, write_json,
};
use super::layout::{
    clean_path, display_path, resolve_absolute_path, runtime_install_root, runtime_meta_path,
    validate_name,
};
use super::now_utc;

fn trim_description(description: Option<String>) -> Option<String> {
    description
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn installed_openclaw_binary_path(install_files: &Path) -> PathBuf {
    install_files.join("node_modules/openclaw/openclaw.mjs")
}

fn installed_openclaw_package_root(install_files: &Path) -> PathBuf {
    install_files.join("node_modules/openclaw")
}

fn openclaw_package_root_from_binary(binary_path: &Path) -> Option<PathBuf> {
    binary_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| *value == "openclaw.mjs")?;
    let package_root = binary_path.parent()?.to_path_buf();
    if package_root.file_name().and_then(|value| value.to_str()) != Some("openclaw") {
        return None;
    }
    if package_root
        .parent()
        .and_then(|value| value.file_name())
        .and_then(|value| value.to_str())
        != Some("node_modules")
    {
        return None;
    }
    Some(package_root)
}

fn symlink_or_copy_dir(source: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        ensure_dir(parent)?;
    }
    #[cfg(unix)]
    {
        let link_target = relative_symlink_target(source, target).unwrap_or_else(|| source.into());
        match std::os::unix::fs::symlink(link_target, target) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(_) => {}
        }
    }
    #[cfg(windows)]
    {
        let link_target = relative_symlink_target(source, target).unwrap_or_else(|| source.into());
        match std::os::windows::fs::symlink_dir(link_target, target) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(_) => {}
        }
    }
    copy_dir_recursive(source, target)
}

fn relative_symlink_target(source: &Path, target: &Path) -> Option<PathBuf> {
    let target_parent = target.parent()?;
    let source_components = source.components().collect::<Vec<_>>();
    let parent_components = target_parent.components().collect::<Vec<_>>();
    let common = source_components
        .iter()
        .zip(&parent_components)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }

    let mut relative = PathBuf::new();
    for _ in common..parent_components.len() {
        relative.push("..");
    }
    for component in &source_components[common..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

fn expose_openclaw_package_runtime_dependencies(install_files: &Path) -> Result<(), String> {
    let package_root = installed_openclaw_package_root(install_files);
    if !package_root.join("package.json").exists() {
        return Ok(());
    }

    let prefix_node_modules = install_files.join("node_modules");
    let package_node_modules = package_root.join("node_modules");
    let entries = match fs::read_dir(&prefix_node_modules) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let source_dir = entry.path();
        let package_name = entry.file_name().to_string_lossy().to_string();
        if package_name == "openclaw" || package_name == ".bin" || package_name.starts_with('.') {
            continue;
        }
        if package_name.starts_with('@') {
            if !source_dir.is_dir() {
                continue;
            }
            for scoped_entry in fs::read_dir(&source_dir).map_err(|error| error.to_string())? {
                let scoped_entry = scoped_entry.map_err(|error| error.to_string())?;
                let scoped_source_dir = scoped_entry.path();
                if !scoped_source_dir.join("package.json").exists() {
                    continue;
                }
                let scoped_name = scoped_entry.file_name().to_string_lossy().to_string();
                let target_dir = package_node_modules.join(&package_name).join(scoped_name);
                if !target_dir.exists() {
                    symlink_or_copy_dir(&scoped_source_dir, &target_dir)?;
                }
            }
            continue;
        }
        if !source_dir.join("package.json").exists() {
            continue;
        }
        let target_dir = package_node_modules.join(package_name);
        if !target_dir.exists() {
            symlink_or_copy_dir(&source_dir, &target_dir)?;
        }
    }
    Ok(())
}

fn openclaw_package_runtime_dependency_layout_issue(package_root: &Path) -> Option<String> {
    if !package_root.join("package.json").exists() {
        return None;
    }
    let Some(prefix_node_modules) = package_root.parent() else {
        return Some(format!(
            "OpenClaw package runtime has no node_modules parent: {}",
            display_path(package_root)
        ));
    };
    let package_node_modules = package_root.join("node_modules");
    let entries = match fs::read_dir(prefix_node_modules) {
        Ok(entries) => entries,
        Err(error) => {
            return Some(format!(
                "failed to inspect OpenClaw package runtime dependencies at {}: {error}",
                display_path(prefix_node_modules)
            ));
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                return Some(format!(
                    "failed to inspect OpenClaw package runtime dependency entry: {error}"
                ));
            }
        };
        let source_dir = entry.path();
        let package_name = entry.file_name().to_string_lossy().to_string();
        if package_name == "openclaw" || package_name == ".bin" || package_name.starts_with('.') {
            continue;
        }
        if package_name.starts_with('@') {
            if !source_dir.is_dir() {
                continue;
            }
            let scoped_entries = match fs::read_dir(&source_dir) {
                Ok(entries) => entries,
                Err(error) => {
                    return Some(format!(
                        "failed to inspect OpenClaw package runtime scoped dependencies at {}: {error}",
                        display_path(&source_dir)
                    ));
                }
            };
            for scoped_entry in scoped_entries {
                let scoped_entry = match scoped_entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        return Some(format!(
                            "failed to inspect OpenClaw package runtime scoped dependency entry: {error}"
                        ));
                    }
                };
                let scoped_source_dir = scoped_entry.path();
                if !scoped_source_dir.join("package.json").exists() {
                    continue;
                }
                let scoped_name = scoped_entry.file_name().to_string_lossy().to_string();
                let target_dir = package_node_modules.join(&package_name).join(scoped_name);
                if !target_dir.join("package.json").exists() {
                    return Some(format!(
                        "OpenClaw package runtime dependency layout is missing {}",
                        display_path(&target_dir.join("package.json"))
                    ));
                }
            }
            continue;
        }
        if !source_dir.join("package.json").exists() {
            continue;
        }
        let target_dir = package_node_modules.join(package_name);
        if !target_dir.join("package.json").exists() {
            return Some(format!(
                "OpenClaw package runtime dependency layout is missing {}",
                display_path(&target_dir.join("package.json"))
            ));
        }
    }
    None
}

#[derive(Clone, Debug, Default)]
struct RuntimeSourceDetails {
    path: Option<PathBuf>,
    url: Option<String>,
    manifest_url: Option<String>,
    sha256: Option<String>,
    integrity: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeReleaseDetails {
    version: Option<String>,
    channel: Option<String>,
    selector_kind: Option<RuntimeReleaseSelectorKind>,
    selector_value: Option<String>,
}

impl RuntimeReleaseDetails {
    pub(crate) fn with_selector(
        selector_kind: Option<RuntimeReleaseSelectorKind>,
        selector_value: Option<String>,
    ) -> Self {
        Self {
            selector_kind,
            selector_value,
            ..Self::default()
        }
    }
}

struct RuntimeInstallTarget {
    name: String,
    final_meta_path: PathBuf,
    final_install_root: PathBuf,
    install_root: PathBuf,
    install_files: PathBuf,
    _lock: ExclusiveFileLock,
}

pub(crate) struct PreparedRuntimeInstall {
    target: Option<RuntimeInstallTarget>,
    meta: RuntimeMeta,
    reused: bool,
}

impl PreparedRuntimeInstall {
    pub(crate) fn reuse(meta: RuntimeMeta) -> Self {
        Self {
            target: None,
            meta,
            reused: true,
        }
    }

    pub(crate) fn meta(&self) -> &RuntimeMeta {
        &self.meta
    }

    pub(crate) fn reused(&self) -> bool {
        self.reused
    }

    pub(crate) fn prepared_binary_path(&self) -> PathBuf {
        self.target
            .as_ref()
            .map(|target| installed_openclaw_binary_path(&target.install_files))
            .unwrap_or_else(|| PathBuf::from(&self.meta.binary_path))
    }

    pub(crate) fn commit(mut self) -> Result<RuntimeMeta, String> {
        let meta = self.meta.clone();
        match self.target.take() {
            Some(target) => publish_runtime(target, meta),
            None => Ok(meta),
        }
    }
}

pub(crate) struct OfficialRuntimeInstallResult {
    pub meta: RuntimeMeta,
}

enum OfficialRuntimeInstallTarget {
    Install(RuntimeInstallTarget),
    Reuse(Box<RuntimeMeta>),
}

impl Drop for RuntimeInstallTarget {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.install_root);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct InstallContext<'a> {
    pub env: &'a BTreeMap<String, String>,
    pub cwd: &'a Path,
}

#[derive(Clone, Debug)]
pub struct BuildLocalRuntimeOptions {
    pub name: String,
    pub repo: String,
    pub description: Option<String>,
    pub force: bool,
    pub include_source_extensions: bool,
}

fn summarize_command_output(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let mut fallback = Vec::new();
    for bytes in [stderr, stdout] {
        let text = String::from_utf8_lossy(bytes);
        let meaningful = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| !line.starts_with("npm notice"))
            .filter(|line| !line.starts_with("npm warn"))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !meaningful.is_empty() {
            let start = meaningful.len().saturating_sub(12);
            return Some(meaningful[start..].join("\n"));
        }
        fallback.extend(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .take(3)
                .map(ToOwned::to_owned),
        );
    }
    (!fallback.is_empty()).then(|| fallback.join("\n"))
}

fn npm_program(env: &BTreeMap<String, String>) -> String {
    configured_npm_program(env).unwrap_or_else(|| "npm".to_string())
}

fn configured_npm_program(env: &BTreeMap<String, String>) -> Option<String> {
    env.get("OCM_INTERNAL_NPM_BIN")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

#[derive(Clone, Debug)]
struct LocalBuildNpmAdapter {
    command: CommandSpec,
    real_npm: String,
    workspace_dependency_dirs: Option<OsString>,
}

#[derive(Clone, Debug)]
struct LocalSourceExtension {
    id: String,
    package_name: String,
    source_dir: PathBuf,
}

#[derive(Clone, Debug)]
struct LocalSourceExtensionArchive {
    extension: LocalSourceExtension,
    archive_path: PathBuf,
}

impl LocalBuildNpmAdapter {
    fn apply_environment(&self, command: &mut Command) {
        command.env("OPENCLAW_OCM_REAL_NPM_BIN", &self.real_npm);
        if let Some(workspace_dependency_dirs) = &self.workspace_dependency_dirs {
            command.env(
                "OPENCLAW_OCM_WORKSPACE_DEPENDENCY_DIRS",
                workspace_dependency_dirs,
            );
        }
    }
}

fn local_workspace_package_dirs(repo_path: &Path) -> Result<Vec<PathBuf>, String> {
    let workspace_path = repo_path.join("pnpm-workspace.yaml");
    let raw = fs::read_to_string(&workspace_path).map_err(|error| {
        format!(
            "failed to read OpenClaw workspace manifest at {}: {error}",
            display_path(&workspace_path)
        )
    })?;
    let value: serde_yaml::Value = serde_yaml::from_str(&raw).map_err(|error| {
        format!(
            "failed to parse OpenClaw workspace manifest at {}: {error}",
            display_path(&workspace_path)
        )
    })?;
    let patterns = value
        .get("packages")
        .and_then(serde_yaml::Value::as_sequence)
        .ok_or_else(|| "OpenClaw pnpm-workspace.yaml is missing a packages list".to_string())?;

    let mut dirs = Vec::new();
    for pattern in patterns {
        let Some(pattern) = pattern.as_str() else {
            continue;
        };
        if pattern == "." {
            dirs.push(repo_path.to_path_buf());
            continue;
        }
        if let Some(parent) = pattern.strip_suffix("/*")
            && !parent.contains(['*', '?', '['])
        {
            let parent_path = repo_path.join(parent);
            if !parent_path.is_dir() {
                continue;
            }
            let entries = fs::read_dir(&parent_path).map_err(|error| {
                format!(
                    "failed to read OpenClaw workspace directory at {}: {error}",
                    display_path(&parent_path)
                )
            })?;
            dirs.extend(
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir()),
            );
            continue;
        }
        if !pattern.contains(['*', '?', '[']) {
            let path = repo_path.join(pattern);
            if path.is_dir() {
                dirs.push(path);
            }
        }
    }
    Ok(dirs)
}

fn workspace_dependency_names(value: &serde_json::Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for section in [
        "dependencies",
        "optionalDependencies",
        "peerDependencies",
        "devDependencies",
    ] {
        let Some(dependencies) = value.get(section).and_then(serde_json::Value::as_object) else {
            continue;
        };
        for (name, spec) in dependencies {
            if spec
                .as_str()
                .is_some_and(|value| value.starts_with("workspace:"))
            {
                names.insert(name.clone());
            }
        }
    }
    names
}

fn local_workspace_dependency_dirs(repo_path: &Path) -> Result<Option<OsString>, String> {
    let package_json_path = repo_path.join("package.json");
    let raw = fs::read_to_string(&package_json_path).map_err(|error| {
        format!(
            "failed to read OpenClaw package.json at {}: {error}",
            display_path(&package_json_path)
        )
    })?;
    let root_package: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
        format!(
            "failed to parse OpenClaw package.json at {}: {error}",
            display_path(&package_json_path)
        )
    })?;

    let mut pending = workspace_dependency_names(&root_package);
    if pending.is_empty() {
        return Ok(None);
    }
    let mut visited = root_package
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .into_iter()
        .collect::<BTreeSet<_>>();

    let mut packages = BTreeMap::new();
    for package_dir in local_workspace_package_dirs(repo_path)? {
        let package_json_path = package_dir.join("package.json");
        let Ok(package_raw) = fs::read_to_string(&package_json_path) else {
            continue;
        };
        let package_value: serde_json::Value =
            serde_json::from_str(&package_raw).map_err(|error| {
                format!(
                    "failed to parse workspace dependency package.json at {}: {error}",
                    display_path(&package_json_path)
                )
            })?;
        if let Some(name) = package_value
            .get("name")
            .and_then(serde_json::Value::as_str)
        {
            packages.insert(name.to_string(), (package_dir, package_value));
        }
    }

    let mut selected = BTreeMap::new();
    while let Some(name) = pending.pop_first() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let (package_dir, package_value) = packages.get(&name).ok_or_else(|| {
            format!(
                "OpenClaw workspace dependency \"{name}\" is not declared by pnpm-workspace.yaml"
            )
        })?;
        selected.insert(name, package_dir.clone());
        pending.extend(workspace_dependency_names(package_value));
    }

    std::env::join_paths(selected.into_values())
        .map(Some)
        .map_err(|error| format!("failed to encode OpenClaw workspace dependency paths: {error}"))
}

fn local_build_npm_adapter(
    repo_path: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Option<LocalBuildNpmAdapter>, String> {
    if configured_npm_program(env).is_some() {
        return Ok(None);
    }

    let adapter_path = ["mts", "mjs"]
        .into_iter()
        .map(|extension| repo_path.join(format!("scripts/ocm-npm-workspace-deps.{extension}")))
        .find(|path| path.is_file());
    let Some(adapter_path) = adapter_path else {
        return Ok(None);
    };

    Ok(Some(LocalBuildNpmAdapter {
        command: CommandSpec {
            program: "node".to_string(),
            args: vec![display_path(&adapter_path)],
            path_prepend: None,
        },
        real_npm: "npm".to_string(),
        workspace_dependency_dirs: local_workspace_dependency_dirs(repo_path)?,
    }))
}

fn install_openclaw_package_with_npm(
    archive_path: &Path,
    additional_archives: &[PathBuf],
    install_files: &Path,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    local_adapter: Option<&LocalBuildNpmAdapter>,
) -> Result<(), String> {
    let host_ready = verify_official_openclaw_runtime_host(env).is_ok();
    let install_command = if let Some(local_adapter) = local_adapter {
        local_adapter.command.clone()
    } else if host_ready {
        CommandSpec {
            program: npm_program(env),
            args: Vec::new(),
            path_prepend: None,
        }
    } else {
        managed_runtime_install_command(env, cwd)?
    };

    let mut command = Command::new(&install_command.program);
    command
        .args(&install_command.args)
        .arg("install")
        .arg("--prefix")
        .arg(install_files)
        .arg("--omit=dev")
        .arg("--no-save")
        .arg("--package-lock=false")
        .args(additional_archives)
        .arg(archive_path)
        .env("npm_config_fund", "false")
        .env("npm_config_audit", "false")
        .env("npm_config_update_notifier", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    install_command.apply_environment(&mut command, env)?;
    if let Some(local_adapter) = local_adapter {
        local_adapter.apply_environment(&mut command);
    }
    let output = command.output().map_err(|error| {
        format!(
            "failed to run {} while installing the OpenClaw package: {error}",
            install_command.program
        )
    })?;

    if output.status.success() {
        return Ok(());
    }

    let detail = summarize_command_output(&output.stdout, &output.stderr).unwrap_or_else(|| {
        format!(
            "{} exited with code {}",
            install_command.program,
            output.status.code().unwrap_or(1)
        )
    });
    Err(format!(
        "failed to install OpenClaw package dependencies with {}: {detail}",
        install_command.program
    ))
}

fn load_openclaw_repo_version(repo_path: &Path) -> Result<String, String> {
    let package_json_path = repo_path.join("package.json");
    let raw = fs::read_to_string(&package_json_path).map_err(|error| {
        format!(
            "failed to read OpenClaw package.json at {}: {error}",
            display_path(&package_json_path)
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
        format!(
            "failed to parse OpenClaw package.json at {}: {error}",
            display_path(&package_json_path)
        )
    })?;

    let package_name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if package_name != "openclaw" {
        return Err(format!(
            "local runtime build requires an OpenClaw repo package named \"openclaw\"; found \"{package_name}\""
        ));
    }

    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "OpenClaw package.json is missing a non-empty version".to_string())
}

fn git_short_commit(repo_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("rev-parse")
        .arg("--short=7")
        .arg("HEAD")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn default_local_build_description(version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("Local OpenClaw build {version} ({commit})"),
        None => format!("Local OpenClaw build {version}"),
    }
}

fn pack_local_openclaw_repo(
    repo_path: &Path,
    pack_dir: &Path,
    env: &BTreeMap<String, String>,
    local_adapter: Option<&LocalBuildNpmAdapter>,
) -> Result<PathBuf, String> {
    ensure_dir(pack_dir)?;
    let npm = local_adapter
        .map(|adapter| adapter.command.clone())
        .unwrap_or_else(|| CommandSpec {
            program: npm_program(env),
            args: Vec::new(),
            path_prepend: None,
        });
    let mut command = Command::new(&npm.program);
    command
        .args(&npm.args)
        .arg("pack")
        .arg("--pack-destination")
        .arg(pack_dir)
        .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0")
        .env("npm_config_fund", "false")
        .env("npm_config_audit", "false")
        .env("npm_config_update_notifier", "false")
        .env_remove("NPM_CONFIG_IGNORE_SCRIPTS")
        .env("npm_config_ignore_scripts", "false")
        .env("PNPM_CONFIG_VERIFY_DEPS_BEFORE_RUN", "false")
        .env("OPENCLAW_PREPACK_ALLOW_UNRELEASED_CHANGELOG", "1")
        .current_dir(repo_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(local_adapter) = local_adapter {
        local_adapter.apply_environment(&mut command);
    }
    let output = command.output().map_err(|error| {
        format!(
            "failed to run npm pack for local OpenClaw build in {}: {error}",
            display_path(repo_path)
        )
    })?;

    if !output.status.success() {
        let detail =
            summarize_command_output(&output.stdout, &output.stderr).unwrap_or_else(|| {
                format!(
                    "npm pack exited with code {}",
                    output.status.code().unwrap_or(1)
                )
            });
        return Err(format!("failed to pack local OpenClaw build: {detail}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines().rev() {
        let trimmed = line.trim();
        if trimmed.ends_with(".tgz") {
            let candidate = pack_dir.join(trimmed);
            if path_exists(&candidate) {
                return Ok(candidate);
            }
        }
    }

    let mut archives = fs::read_dir(pack_dir)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("tgz"))
        .collect::<Vec<_>>();
    archives.sort();
    match archives.len() {
        1 => Ok(archives.remove(0)),
        0 => Err("npm pack did not produce an OpenClaw package archive".to_string()),
        _ => Err(format!(
            "npm pack produced multiple package archives in {}; expected one",
            display_path(pack_dir)
        )),
    }
}

fn packaged_openclaw_extension_ids(archive_path: &Path) -> Result<BTreeSet<String>, String> {
    let file = fs::File::open(archive_path).map_err(|error| {
        format!(
            "failed to inspect local OpenClaw package at {}: {error}",
            display_path(archive_path)
        )
    })?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut ids = BTreeSet::new();
    for entry in archive.entries().map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path().map_err(|error| error.to_string())?;
        let mut components = path.components();
        if !matches!(components.next(), Some(Component::Normal(value)) if value == "package")
            || !matches!(components.next(), Some(Component::Normal(value)) if value == "dist")
            || !matches!(components.next(), Some(Component::Normal(value)) if value == "extensions")
        {
            continue;
        }
        if let Some(Component::Normal(value)) = components.next()
            && value != "node_modules"
        {
            ids.insert(value.to_string_lossy().to_string());
        }
    }
    Ok(ids)
}

fn npm_package_relative_path(package_name: &str) -> Option<PathBuf> {
    let valid_component = |value: &str| {
        !value.is_empty()
            && value != "."
            && value != ".."
            && !value.contains(['/', '\\'])
            && Path::new(value).file_name() == Some(OsStr::new(value))
    };
    if let Some(scoped) = package_name.strip_prefix('@') {
        let (scope, name) = scoped.split_once('/')?;
        if !valid_component(scope) || !valid_component(name) || name.contains('/') {
            return None;
        }
        return Some(PathBuf::from(format!("@{scope}")).join(name));
    }
    valid_component(package_name).then(|| PathBuf::from(package_name))
}

fn local_source_extensions_omitted_from_package(
    repo_path: &Path,
    archive_path: &Path,
) -> Result<Vec<LocalSourceExtension>, String> {
    let packaged_ids = packaged_openclaw_extension_ids(archive_path)?;
    let extensions_dir = repo_path.join("dist/extensions");
    let entries = fs::read_dir(&extensions_dir).map_err(|error| {
        format!(
            "failed to read built OpenClaw extensions at {}: {error}",
            display_path(&extensions_dir)
        )
    })?;
    let mut extensions = Vec::new();
    let mut package_names = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let source_dir = entry.path();
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if id == "node_modules" || packaged_ids.contains(&id) {
            continue;
        }
        let package_json_path = source_dir.join("package.json");
        let manifest_path = source_dir.join("openclaw.plugin.json");
        if !package_json_path.exists() && !manifest_path.exists() {
            continue;
        }
        let raw = fs::read_to_string(&package_json_path).map_err(|error| {
            format!(
                "source extension \"{id}\" cannot be packaged because {} is unavailable: {error}",
                display_path(&package_json_path)
            )
        })?;
        let package_json: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
            format!(
                "failed to parse source extension package.json at {}: {error}",
                display_path(&package_json_path)
            )
        })?;
        let package_name = package_json
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| npm_package_relative_path(value).is_some())
            .ok_or_else(|| {
                format!(
                    "source extension \"{id}\" has no valid npm package name in {}",
                    display_path(&package_json_path)
                )
            })?
            .to_string();
        if !package_names.insert(package_name.clone()) {
            return Err(format!(
                "multiple source extensions use npm package name \"{package_name}\""
            ));
        }
        extensions.push(LocalSourceExtension {
            id,
            package_name,
            source_dir,
        });
    }
    extensions.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(extensions)
}

fn pack_local_source_extensions(
    extensions: Vec<LocalSourceExtension>,
    pack_dir: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Vec<LocalSourceExtensionArchive>, String> {
    if extensions.is_empty() {
        return Ok(Vec::new());
    }
    let npm = CommandSpec {
        program: npm_program(env),
        args: Vec::new(),
        path_prepend: None,
    };
    let mut command = Command::new(&npm.program);
    command
        .args(&npm.args)
        .arg("pack")
        .arg("--json")
        .arg("--ignore-scripts")
        .arg("--pack-destination")
        .arg(pack_dir)
        .args(extensions.iter().map(|extension| &extension.source_dir))
        .env("npm_config_fund", "false")
        .env("npm_config_audit", "false")
        .env("npm_config_update_notifier", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    npm.apply_environment(&mut command, env)?;
    let output = command.output().map_err(|error| {
        format!("failed to run npm pack for local OpenClaw source extensions: {error}")
    })?;
    if !output.status.success() {
        let detail =
            summarize_command_output(&output.stdout, &output.stderr).unwrap_or_else(|| {
                format!(
                    "npm pack exited with code {}",
                    output.status.code().unwrap_or(1)
                )
            });
        return Err(format!(
            "failed to pack local OpenClaw source extensions: {detail}"
        ));
    }

    let values: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("failed to parse npm pack source extension output: {error}"))?;
    let values = values
        .as_array()
        .ok_or_else(|| "npm pack source extension output was not a JSON array".to_string())?;
    let mut archives_by_name = BTreeMap::new();
    for value in values {
        let package_name = value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "npm pack source extension output is missing a package name".to_string()
            })?;
        let filename = value
            .get("filename")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "npm pack source extension output is missing a filename".to_string())?;
        if Path::new(filename).file_name() != Some(OsStr::new(filename)) {
            return Err(format!(
                "npm pack returned an invalid source extension filename: {filename}"
            ));
        }
        let archive_path = pack_dir.join(filename);
        if !path_exists(&archive_path) {
            return Err(format!(
                "npm pack did not create source extension archive {}",
                display_path(&archive_path)
            ));
        }
        if archives_by_name
            .insert(package_name.to_string(), archive_path)
            .is_some()
        {
            return Err(format!(
                "npm pack returned duplicate source extension package \"{package_name}\""
            ));
        }
    }

    let archives = extensions
        .into_iter()
        .map(|extension| {
            let archive_path = archives_by_name
                .remove(&extension.package_name)
                .ok_or_else(|| {
                    format!(
                        "npm pack did not return source extension package \"{}\"",
                        extension.package_name
                    )
                })?;
            Ok(LocalSourceExtensionArchive {
                extension,
                archive_path,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if let Some(package_name) = archives_by_name.keys().next() {
        return Err(format!(
            "npm pack returned unexpected source extension package \"{package_name}\""
        ));
    }
    Ok(archives)
}

fn materialize_local_source_extensions(
    install_files: &Path,
    extensions: &[LocalSourceExtensionArchive],
) -> Result<(), String> {
    let target_root = installed_openclaw_package_root(install_files).join("dist/extensions");
    for extension in extensions {
        let package_relative_path = npm_package_relative_path(&extension.extension.package_name)
            .ok_or_else(|| {
                format!(
                    "source extension \"{}\" has an invalid npm package name",
                    extension.extension.id
                )
            })?;
        let installed_package = install_files
            .join("node_modules")
            .join(package_relative_path);
        if !installed_package.join("package.json").exists() {
            return Err(format!(
                "installed source extension \"{}\" is missing {}",
                extension.extension.id,
                display_path(&installed_package.join("package.json"))
            ));
        }
        let target = target_root.join(&extension.extension.id);
        if path_exists(&target) {
            return Err(format!(
                "source extension target already exists after package installation: {}",
                display_path(&target)
            ));
        }
        copy_dir_recursive(&installed_package, &target)?;
    }
    Ok(())
}

fn build_installed_runtime_meta(
    target: &RuntimeInstallTarget,
    binary_path: &Path,
    source: &RuntimeSourceDetails,
    runtime_sha256: Option<String>,
    release: &RuntimeReleaseDetails,
    description: Option<String>,
) -> RuntimeMeta {
    let final_binary_path = binary_path
        .strip_prefix(&target.install_root)
        .map(|relative| target.final_install_root.join(relative))
        .unwrap_or_else(|_| binary_path.to_path_buf());
    let created_at = now_utc();
    RuntimeMeta {
        kind: "ocm-runtime".to_string(),
        name: target.name.clone(),
        binary_path: display_path(&final_binary_path),
        source_kind: RuntimeSourceKind::Installed,
        source_path: source.path.as_deref().map(display_path),
        source_url: source.url.clone(),
        source_manifest_url: source.manifest_url.clone(),
        source_sha256: source.sha256.clone(),
        source_integrity: source.integrity.clone(),
        runtime_sha256,
        release_version: release.version.clone(),
        release_channel: release.channel.clone(),
        release_selector_kind: release.selector_kind.clone(),
        release_selector_value: release.selector_value.clone(),
        install_root: Some(display_path(&target.final_install_root)),
        description,
        created_at,
        updated_at: created_at,
    }
}

fn copy_installed_runtime_binary(source_path: &Path, binary_path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(source_path).map_err(|error| error.to_string())?;
    fs::copy(source_path, binary_path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        let permissions = metadata.permissions();
        fs::set_permissions(binary_path, permissions).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn prepare_runtime_at_path(
    target: RuntimeInstallTarget,
    file_name: &Path,
    source: RuntimeSourceDetails,
    release: RuntimeReleaseDetails,
    description: Option<String>,
) -> Result<PreparedRuntimeInstall, String> {
    if path_exists(&target.install_root) {
        return Err(format!(
            "runtime install root already exists: {}",
            display_path(&target.install_root)
        ));
    }

    (|| {
        ensure_dir(&target.install_files)?;
        let binary_path = target.install_files.join(file_name);
        match (source.path.as_deref(), source.url.as_deref()) {
            (Some(source_path), _) => copy_installed_runtime_binary(source_path, &binary_path)?,
            (None, Some(source_url)) => {
                download_to_file(source_url, &binary_path)?;
                if let Some(source_sha256) = source.sha256.as_deref() {
                    verify_file_sha256(&binary_path, source_sha256)?;
                }
                #[cfg(unix)]
                {
                    let mut permissions = fs::metadata(&binary_path)
                        .map_err(|error| error.to_string())?
                        .permissions();
                    permissions.set_mode(0o755);
                    fs::set_permissions(&binary_path, permissions)
                        .map_err(|error| error.to_string())?;
                }
            }
            (None, None) => return Err("runtime install requires a source path or URL".to_string()),
        }

        let meta = build_installed_runtime_meta(
            &target,
            &binary_path,
            &source,
            None,
            &release,
            description,
        );
        Ok(PreparedRuntimeInstall {
            target: Some(target),
            meta,
            reused: false,
        })
    })()
}

fn prepare_runtime_install_target(
    name: String,
    replace_existing: bool,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RuntimeInstallTarget, String> {
    let lock = lock_runtime(&name, env, cwd)?;
    prepare_runtime_install_target_with_lock(name, replace_existing, env, cwd, lock)
}

fn prepare_runtime_install_target_with_lock(
    name: String,
    replace_existing: bool,
    env: &BTreeMap<String, String>,
    cwd: &Path,
    lock: ExclusiveFileLock,
) -> Result<RuntimeInstallTarget, String> {
    let final_meta_path = runtime_meta_path(&name, env, cwd)?;
    let final_install_root = runtime_install_root(&name, env, cwd)?;
    let parent = final_install_root
        .parent()
        .ok_or_else(|| format!("runtime install root has no parent: {name}"))?;
    if path_exists(&final_meta_path) && !replace_existing {
        return Err(format!("runtime \"{name}\" already exists"));
    }
    let install_root = parent.join(format!(
        ".{name}.stage-{}-{}",
        std::process::id(),
        now_utc().unix_timestamp_nanos()
    ));
    let _ = fs::remove_dir_all(&install_root);
    let install_files = install_root.join("files");
    Ok(RuntimeInstallTarget {
        name,
        final_meta_path,
        final_install_root,
        install_root,
        install_files,
        _lock: lock,
    })
}

fn prepare_official_runtime_install_target(
    name: String,
    force: bool,
    source: &RuntimeSourceDetails,
    release: &RuntimeReleaseDetails,
    context: InstallContext<'_>,
) -> Result<OfficialRuntimeInstallTarget, String> {
    let lock = lock_runtime(&name, context.env, context.cwd)?;
    let meta_path = runtime_meta_path(&name, context.env, context.cwd)?;
    if path_exists(&meta_path) && !force {
        let existing = get_runtime(&name, context.env, context.cwd)?;
        let same_release = existing.release_version == release.version
            && existing.release_channel == release.channel
            && existing.release_selector_kind == release.selector_kind
            && existing.release_selector_value == release.selector_value
            && existing.source_url == source.url
            && existing.source_manifest_url == source.manifest_url
            && existing.source_integrity == source.integrity;
        if same_release && runtime_integrity_issue(&existing, context.env).is_none() {
            return Ok(OfficialRuntimeInstallTarget::Reuse(Box::new(existing)));
        }
        return Err(format!("runtime \"{name}\" already exists"));
    }

    prepare_runtime_install_target_with_lock(name, force, context.env, context.cwd, lock)
        .map(OfficialRuntimeInstallTarget::Install)
}

fn lock_runtime(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ExclusiveFileLock, String> {
    let install_root = runtime_install_root(name, env, cwd)?;
    let parent = install_root
        .parent()
        .ok_or_else(|| format!("runtime install root has no parent: {name}"))?;
    ensure_dir(parent)?;
    lock_file(
        &parent.join(format!(".{name}.lock")),
        "runtime installation",
    )
}

fn prepare_runtime_from_openclaw_package(
    target: RuntimeInstallTarget,
    source: RuntimeSourceDetails,
    release: RuntimeReleaseDetails,
    description: Option<String>,
    context: InstallContext<'_>,
) -> Result<PreparedRuntimeInstall, String> {
    if path_exists(&target.install_root) {
        return Err(format!(
            "runtime install root already exists: {}",
            display_path(&target.install_root)
        ));
    }
    (|| {
        ensure_dir(&target.install_files)?;
        let tarball_url = source.url.as_deref().ok_or_else(|| {
            "official OpenClaw runtime install requires a tarball URL".to_string()
        })?;
        let source_integrity = source.integrity.as_deref().ok_or_else(|| {
            "official OpenClaw runtime install requires source integrity".to_string()
        })?;
        let archive_name = artifact_file_name_from_url(tarball_url)?;
        let archive_path = target.install_files.join(&archive_name);
        download_to_file(tarball_url, &archive_path)?;
        verify_file_integrity(&archive_path, source_integrity)?;

        let meta = stage_runtime_from_openclaw_package_archive(
            &target,
            &archive_path,
            source,
            release,
            description,
            context,
            None,
            &[],
        );
        let _ = fs::remove_file(&archive_path);
        Ok(PreparedRuntimeInstall {
            target: Some(target),
            meta: meta?,
            reused: false,
        })
    })()
}

fn stage_runtime_from_openclaw_package_archive(
    target: &RuntimeInstallTarget,
    archive_path: &Path,
    source: RuntimeSourceDetails,
    release: RuntimeReleaseDetails,
    description: Option<String>,
    context: InstallContext<'_>,
    local_adapter: Option<&LocalBuildNpmAdapter>,
    source_extensions: &[LocalSourceExtensionArchive],
) -> Result<RuntimeMeta, String> {
    let additional_archives = source_extensions
        .iter()
        .map(|extension| extension.archive_path.clone())
        .collect::<Vec<_>>();
    install_openclaw_package_with_npm(
        archive_path,
        &additional_archives,
        &target.install_files,
        context.cwd,
        context.env,
        local_adapter,
    )?;
    materialize_local_source_extensions(&target.install_files, source_extensions)?;
    expose_openclaw_package_runtime_dependencies(&target.install_files)?;

    let binary_path = installed_openclaw_binary_path(&target.install_files);
    if !path_exists(&binary_path) {
        let release_version = release.version.as_deref().unwrap_or("unknown");
        return Err(format!(
            "OpenClaw package \"{release_version}\" is missing node_modules/openclaw/openclaw.mjs after installation"
        ));
    }
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&binary_path)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary_path, permissions).map_err(|error| error.to_string())?;
    }
    let binary_sha256 = file_sha256(&binary_path)?;
    let runtime_sha256 = tree_sha256(&target.install_files.join("node_modules"))?;

    let mut source = source;
    source.sha256 = Some(binary_sha256);
    Ok(build_installed_runtime_meta(
        target,
        &binary_path,
        &source,
        Some(runtime_sha256),
        &release,
        description,
    ))
}

fn publish_runtime(target: RuntimeInstallTarget, meta: RuntimeMeta) -> Result<RuntimeMeta, String> {
    let nonce = now_utc().unix_timestamp_nanos();
    let backup_root = target
        .final_install_root
        .with_file_name(format!(".{}.backup-{nonce}", target.name));
    let backup_meta = target
        .final_meta_path
        .with_file_name(format!(".{}.json.backup-{nonce}", target.name));
    let had_root = path_exists(&target.final_install_root);
    let had_meta = path_exists(&target.final_meta_path);

    if had_root {
        fs::rename(&target.final_install_root, &backup_root).map_err(|error| {
            format!(
                "failed to preserve runtime \"{}\" before replacement: {error}",
                target.name
            )
        })?;
    }
    if had_meta && let Err(error) = fs::rename(&target.final_meta_path, &backup_meta) {
        if had_root
            && let Err(rollback_error) = fs::rename(&backup_root, &target.final_install_root)
        {
            return Err(format!(
                "failed to preserve runtime \"{}\" metadata before replacement: {error}; failed to restore its install root from {}: {rollback_error}",
                target.name,
                display_path(&backup_root)
            ));
        }
        return Err(format!(
            "failed to preserve runtime \"{}\" metadata before replacement: {error}",
            target.name
        ));
    }

    let publish_result = (|| {
        fs::rename(&target.install_root, &target.final_install_root).map_err(|error| {
            format!(
                "failed to publish runtime \"{}\" install root: {error}",
                target.name
            )
        })?;
        write_json(&target.final_meta_path, &meta).map_err(|error| {
            format!(
                "failed to publish runtime \"{}\" metadata: {error}",
                target.name
            )
        })
    })();

    if let Err(error) = publish_result {
        let _ = fs::remove_dir_all(&target.final_install_root);
        let _ = fs::remove_file(&target.final_meta_path);
        let mut rollback_errors = Vec::new();
        if had_root
            && let Err(rollback_error) = fs::rename(&backup_root, &target.final_install_root)
        {
            rollback_errors.push(format!("install root: {rollback_error}"));
        }
        if had_meta && let Err(rollback_error) = fs::rename(&backup_meta, &target.final_meta_path) {
            rollback_errors.push(format!("metadata: {rollback_error}"));
        }
        if rollback_errors.is_empty() {
            return Err(error);
        }
        return Err(format!(
            "{error}; failed to restore previous runtime: {}",
            rollback_errors.join("; ")
        ));
    }

    let _ = fs::remove_dir_all(&backup_root);
    let _ = fs::remove_file(&backup_meta);
    Ok(meta)
}

pub fn list_runtimes(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Vec<RuntimeMeta>, String> {
    let stores = super::ensure_store(env, cwd)?;
    let files = load_json_files(&stores.runtimes_dir)?;
    let mut out: Vec<RuntimeMeta> = Vec::with_capacity(files.len());
    for file in files {
        out.push(read_json(&file)?);
    }
    out.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(out)
}

pub fn get_runtime(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RuntimeMeta, String> {
    let safe_name = validate_name(name, "Runtime name")?;
    let path = runtime_meta_path(&safe_name, env, cwd)?;
    if !path_exists(&path) {
        return Err(format!("runtime \"{safe_name}\" does not exist"));
    }
    read_json(&path)
}

pub fn get_runtime_verified(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RuntimeMeta, String> {
    verify_runtime_binary(get_runtime(name, env, cwd)?, env)
}

pub fn add_runtime(
    options: AddRuntimeOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RuntimeMeta, String> {
    let name = validate_name(&options.name, "Runtime name")?;
    let _lock = lock_runtime(&name, env, cwd)?;
    let meta_path = runtime_meta_path(&name, env, cwd)?;
    if path_exists(&meta_path) {
        return Err(format!("runtime \"{name}\" already exists"));
    }

    let raw_path = options.path.trim();
    if raw_path.is_empty() {
        return Err("runtime path is required".to_string());
    }

    let binary_path = resolve_absolute_path(raw_path, env, cwd)?;
    if !path_exists(&binary_path) {
        return Err(format!(
            "runtime path does not exist: {}",
            display_path(&binary_path)
        ));
    }

    let metadata = fs::metadata(&binary_path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err(format!(
            "runtime path must be a file: {}",
            display_path(&binary_path)
        ));
    }

    let description = trim_description(options.description);

    let created_at = now_utc();
    let meta = RuntimeMeta {
        kind: "ocm-runtime".to_string(),
        name,
        binary_path: display_path(&binary_path),
        source_kind: RuntimeSourceKind::Registered,
        source_path: Some(display_path(&binary_path)),
        source_url: None,
        source_manifest_url: None,
        source_sha256: None,
        source_integrity: None,
        runtime_sha256: None,
        release_version: None,
        release_channel: None,
        release_selector_kind: None,
        release_selector_value: None,
        install_root: None,
        description,
        created_at,
        updated_at: created_at,
    };
    write_json(&meta_path, &meta)?;
    Ok(meta)
}

pub fn remove_runtime(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RuntimeMeta, String> {
    let name = validate_name(name, "Runtime name")?;
    let _lock = lock_runtime(&name, env, cwd)?;
    let meta = get_runtime(&name, env, cwd)?;
    let path = runtime_meta_path(&meta.name, env, cwd)?;
    if let Some(install_root) = meta.install_root.as_deref() {
        let expected_root = runtime_install_root(&meta.name, env, cwd)?;
        if clean_path(Path::new(install_root)) == expected_root && path_exists(&expected_root) {
            fs::remove_dir_all(&expected_root).map_err(|error| error.to_string())?;
        }
    }
    fs::remove_file(path).map_err(|error| error.to_string())?;
    Ok(meta)
}

pub fn install_runtime(
    options: InstallRuntimeOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RuntimeMeta, String> {
    let name = validate_name(&options.name, "Runtime name")?;
    let target = prepare_runtime_install_target(name, options.force, env, cwd)?;

    let raw_path = options.path.trim();
    if raw_path.is_empty() {
        return Err("runtime path is required".to_string());
    }

    let source_path = resolve_absolute_path(raw_path, env, cwd)?;
    if !path_exists(&source_path) {
        return Err(format!(
            "runtime path does not exist: {}",
            display_path(&source_path)
        ));
    }

    let metadata = fs::metadata(&source_path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err(format!(
            "runtime path must be a file: {}",
            display_path(&source_path)
        ));
    }

    let file_name = PathBuf::from(source_path.file_name().ok_or_else(|| {
        format!(
            "runtime path must include a file name: {}",
            display_path(&source_path)
        )
    })?);
    prepare_runtime_at_path(
        target,
        &file_name,
        RuntimeSourceDetails {
            path: Some(source_path),
            ..RuntimeSourceDetails::default()
        },
        RuntimeReleaseDetails::default(),
        trim_description(options.description),
    )?
    .commit()
}

pub fn install_runtime_from_url(
    options: InstallRuntimeFromUrlOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RuntimeMeta, String> {
    let name = validate_name(&options.name, "Runtime name")?;
    let target = prepare_runtime_install_target(name, options.force, env, cwd)?;

    let file_name = artifact_file_name_from_url(&options.url)?;
    prepare_runtime_at_path(
        target,
        Path::new(&file_name),
        RuntimeSourceDetails {
            url: Some(options.url),
            ..RuntimeSourceDetails::default()
        },
        RuntimeReleaseDetails::default(),
        trim_description(options.description),
    )?
    .commit()
}

pub(crate) fn install_runtime_from_local_openclaw_build(
    options: BuildLocalRuntimeOptions,
    context: InstallContext<'_>,
) -> Result<RuntimeMeta, String> {
    let name = validate_name(&options.name, "Runtime name")?;
    let meta_path = runtime_meta_path(&name, context.env, context.cwd)?;
    if path_exists(&meta_path) && !options.force {
        return Err(format!("runtime \"{name}\" already exists"));
    }

    let raw_repo = options.repo.trim();
    if raw_repo.is_empty() {
        return Err("OpenClaw repo path is required".to_string());
    }
    let repo_path = resolve_absolute_path(raw_repo, context.env, context.cwd)?;
    let metadata = fs::metadata(&repo_path).map_err(|error| {
        format!(
            "OpenClaw repo path does not exist: {} ({error})",
            display_path(&repo_path)
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "OpenClaw repo path must be a directory: {}",
            display_path(&repo_path)
        ));
    }

    let repo_path = fs::canonicalize(&repo_path).map_err(|error| error.to_string())?;
    let version = load_openclaw_repo_version(&repo_path)?;
    let commit = git_short_commit(&repo_path);
    let stores = super::ensure_store(context.env, context.cwd)?;
    let pack_dir = stores.runtimes_dir.join(format!(
        ".{name}.pack-{}-{}",
        std::process::id(),
        now_utc().unix_timestamp_nanos()
    ));
    let _ = fs::remove_dir_all(&pack_dir);

    let result = (|| {
        let local_adapter = local_build_npm_adapter(&repo_path, context.env)?;
        let archive_path =
            pack_local_openclaw_repo(&repo_path, &pack_dir, context.env, local_adapter.as_ref())?;
        let source_extensions = if options.include_source_extensions {
            let extensions =
                local_source_extensions_omitted_from_package(&repo_path, &archive_path)?;
            pack_local_source_extensions(extensions, &pack_dir, context.env)?
        } else {
            Vec::new()
        };
        let target = prepare_runtime_install_target(name, options.force, context.env, context.cwd)?;
        if path_exists(&target.install_root) {
            return Err(format!(
                "runtime install root already exists: {}",
                display_path(&target.install_root)
            ));
        }
        ensure_dir(&target.install_files)?;
        let description = trim_description(options.description)
            .or_else(|| Some(default_local_build_description(&version, commit.as_deref())));
        let meta = stage_runtime_from_openclaw_package_archive(
            &target,
            &archive_path,
            RuntimeSourceDetails {
                path: Some(repo_path),
                ..RuntimeSourceDetails::default()
            },
            RuntimeReleaseDetails {
                version: Some(version),
                ..RuntimeReleaseDetails::default()
            },
            description,
            context,
            local_adapter.as_ref(),
            &source_extensions,
        )?;
        publish_runtime(target, meta)
    })();

    let _ = fs::remove_dir_all(&pack_dir);
    result
}

pub fn install_runtime_from_release(
    options: InstallRuntimeFromReleaseOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RuntimeMeta, String> {
    let manifest = load_release_manifest(&options.manifest_url)?;
    let release = select_release(
        &manifest,
        options.version.as_deref(),
        options.channel.as_deref(),
    )?;
    let (selector_kind, selector_value) =
        match (options.version.as_deref(), options.channel.as_deref()) {
            (Some(version), None) => (
                Some(RuntimeReleaseSelectorKind::Version),
                Some(version.trim().to_string()),
            ),
            (None, Some(channel)) => (
                Some(RuntimeReleaseSelectorKind::Channel),
                Some(channel.trim().to_string()),
            ),
            _ => (None, None),
        };
    install_runtime_from_selected_release(
        options.name,
        options.force,
        options.manifest_url,
        release,
        selector_kind,
        selector_value,
        options.description,
        env,
        cwd,
    )
}

pub(crate) fn install_runtime_from_selected_release(
    name: String,
    force: bool,
    manifest_url: String,
    release: RuntimeRelease,
    selector_kind: Option<RuntimeReleaseSelectorKind>,
    selector_value: Option<String>,
    description: Option<String>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RuntimeMeta, String> {
    prepare_runtime_from_selected_release(
        name,
        force,
        manifest_url,
        release,
        selector_kind,
        selector_value,
        description,
        env,
        cwd,
    )?
    .commit()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_runtime_from_selected_release(
    name: String,
    force: bool,
    manifest_url: String,
    release: RuntimeRelease,
    selector_kind: Option<RuntimeReleaseSelectorKind>,
    selector_value: Option<String>,
    description: Option<String>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PreparedRuntimeInstall, String> {
    let name = validate_name(&name, "Runtime name")?;
    let source_sha256 = release
        .sha256
        .as_deref()
        .ok_or_else(|| {
            format!(
                "runtime release \"{}\" is missing required sha256 integrity",
                release.version
            )
        })
        .and_then(normalize_sha256)?;
    let target = prepare_runtime_install_target(name, force, env, cwd)?;
    let release_details = RuntimeReleaseDetails {
        version: Some(release.version.clone()),
        channel: release.channel.clone(),
        selector_kind,
        selector_value,
    };
    let description =
        trim_description(description).or_else(|| trim_description(release.description));

    let file_name = artifact_file_name_from_url(&release.url)?;
    prepare_runtime_at_path(
        target,
        Path::new(&file_name),
        RuntimeSourceDetails {
            url: Some(release.url),
            manifest_url: Some(manifest_url),
            sha256: Some(source_sha256),
            ..RuntimeSourceDetails::default()
        },
        release_details,
        description,
    )
}

pub fn install_runtime_from_official_openclaw_release(
    options: InstallRuntimeFromOfficialReleaseOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RuntimeMeta, String> {
    let name = validate_name(&options.name, "Runtime name")?;
    let channel = options
        .channel
        .as_deref()
        .map(normalize_openclaw_channel_selector)
        .transpose()?;

    let releases_url = official_openclaw_releases_url(env);
    let releases = load_official_openclaw_release_selection(&releases_url)?;
    let (release_selector_kind, release_selector_value) =
        match (options.version.as_deref(), channel.as_deref()) {
            (Some(version), None) => (
                Some(RuntimeReleaseSelectorKind::Version),
                Some(version.trim().to_string()),
            ),
            (None, Some(channel)) => (
                Some(RuntimeReleaseSelectorKind::Channel),
                Some(channel.trim().to_string()),
            ),
            _ => (None, None),
        };
    let release = match (options.version.as_deref(), channel.as_deref()) {
        (Some(version), None) => select_official_openclaw_release_by_version(&releases, version)?,
        (None, Some(channel)) => select_official_openclaw_release_by_channel(&releases, channel)?,
        (Some(_), Some(_)) => {
            return Err("runtime install accepts only one of --version or --channel".to_string());
        }
        (None, None) => {
            return Err("runtime install requires --version or --channel".to_string());
        }
    };
    let description = trim_description(options.description)
        .or_else(|| Some(format!("Official OpenClaw release {}", release.version)));

    install_runtime_from_selected_official_openclaw_release(
        name,
        options.force,
        releases_url,
        release,
        RuntimeReleaseDetails {
            selector_kind: release_selector_kind,
            selector_value: release_selector_value,
            ..RuntimeReleaseDetails::default()
        },
        description,
        InstallContext { env, cwd },
    )
    .map(|result| result.meta)
}

pub(crate) fn install_runtime_from_selected_official_openclaw_release(
    name: String,
    force: bool,
    releases_url: String,
    release: OpenClawRelease,
    release_details: RuntimeReleaseDetails,
    description: Option<String>,
    context: InstallContext<'_>,
) -> Result<OfficialRuntimeInstallResult, String> {
    let prepared = prepare_runtime_from_selected_official_openclaw_release(
        name,
        force,
        releases_url,
        release,
        release_details,
        description,
        context,
    )?;
    prepared
        .commit()
        .map(|meta| OfficialRuntimeInstallResult { meta })
}

pub(crate) fn prepare_runtime_from_selected_official_openclaw_release(
    name: String,
    force: bool,
    releases_url: String,
    release: OpenClawRelease,
    release_details: RuntimeReleaseDetails,
    description: Option<String>,
    context: InstallContext<'_>,
) -> Result<PreparedRuntimeInstall, String> {
    let source_integrity = release
        .integrity
        .as_deref()
        .ok_or_else(|| {
            format!(
                "official OpenClaw release \"{}\" is missing required sha512 integrity",
                release.version
            )
        })
        .and_then(normalize_file_integrity)?;
    let description = trim_description(description)
        .or_else(|| Some(format!("Official OpenClaw release {}", release.version)));
    let source = RuntimeSourceDetails {
        url: Some(release.tarball_url),
        manifest_url: Some(releases_url),
        integrity: Some(source_integrity),
        ..RuntimeSourceDetails::default()
    };
    let release = RuntimeReleaseDetails {
        version: Some(release.version),
        channel: release.channel,
        selector_kind: release_details.selector_kind,
        selector_value: release_details.selector_value,
    };
    match prepare_official_runtime_install_target(name, force, &source, &release, context)? {
        OfficialRuntimeInstallTarget::Reuse(meta) => Ok(PreparedRuntimeInstall {
            target: None,
            meta: *meta,
            reused: true,
        }),
        OfficialRuntimeInstallTarget::Install(target) => {
            prepare_runtime_from_openclaw_package(target, source, release, description, context)
        }
    }
}

fn runtime_execution_issue(meta: &RuntimeMeta, env: &BTreeMap<String, String>) -> Option<String> {
    let binary_path = Path::new(&meta.binary_path);
    if !path_exists(binary_path) {
        return Some(format!(
            "binary path does not exist: {}",
            display_path(binary_path)
        ));
    }

    let metadata = match fs::metadata(binary_path) {
        Ok(metadata) => metadata,
        Err(error) => return Some(error.to_string()),
    };
    if !metadata.is_file() {
        return Some(format!(
            "binary path is not a file: {}",
            display_path(binary_path)
        ));
    }

    if let Some(expected_sha256) = meta.source_sha256.as_deref() {
        let expected_sha256 = match normalize_sha256(expected_sha256) {
            Ok(value) => value,
            Err(error) => return Some(error),
        };
        let actual_sha256 = match file_sha256(binary_path) {
            Ok(value) => value,
            Err(error) => return Some(error),
        };
        if actual_sha256 != expected_sha256 {
            return Some(format!(
                "sha256 mismatch: expected {expected_sha256}, got {actual_sha256}"
            ));
        }
    }

    if (is_official_openclaw_package_runtime(meta, env) || is_openclaw_package_runtime(meta))
        && let Some(package_root) = openclaw_package_root_from_binary(binary_path)
        && let Some(issue) = openclaw_package_runtime_dependency_layout_issue(&package_root)
    {
        return Some(issue);
    }

    None
}

pub fn runtime_integrity_issue(
    meta: &RuntimeMeta,
    env: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(issue) = runtime_execution_issue(meta, env) {
        return Some(issue);
    }

    if let Some(expected_runtime_sha256) = meta.runtime_sha256.as_deref() {
        let expected_runtime_sha256 = match normalize_sha256(expected_runtime_sha256) {
            Ok(value) => value,
            Err(error) => return Some(error),
        };
        let Some(install_root) = meta.install_root.as_deref() else {
            return Some("runtime sha256 requires an install root".to_string());
        };
        let runtime_root = Path::new(install_root).join("files/node_modules");
        let actual_runtime_sha256 = match tree_sha256(&runtime_root) {
            Ok(value) => value,
            Err(error) => return Some(error),
        };
        if actual_runtime_sha256 != expected_runtime_sha256 {
            return Some(format!(
                "runtime sha256 mismatch: expected {expected_runtime_sha256}, got {actual_runtime_sha256}"
            ));
        }
    }

    None
}

pub fn verify_runtime_binary(
    meta: RuntimeMeta,
    env: &BTreeMap<String, String>,
) -> Result<RuntimeMeta, String> {
    if let Some(issue) = runtime_execution_issue(&meta, env) {
        return Err(format!("runtime \"{}\" {issue}", meta.name));
    }

    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::summarize_command_output;

    #[test]
    fn command_summary_prefers_errors_over_npm_warnings() {
        let stderr = br#"
npm warn deprecated node-domexception@1.0.0: Use your platform's native DOMException instead
npm error code 1
npm error command failed
npm error Error [ERR_MODULE_NOT_FOUND]: Cannot find module './missing.mjs'
"#;

        let summary = summarize_command_output(b"", stderr).unwrap();

        assert!(summary.contains("npm error code 1"));
        assert!(summary.contains("ERR_MODULE_NOT_FOUND"));
        assert!(!summary.contains("deprecated node-domexception"));
    }

    #[test]
    fn command_summary_falls_back_to_warnings_when_no_errors_exist() {
        let stderr = br#"
npm warn deprecated node-domexception@1.0.0: Use your platform's native DOMException instead
"#;

        let summary = summarize_command_output(b"", stderr).unwrap();

        assert!(summary.contains("deprecated node-domexception"));
    }

    #[test]
    fn command_summary_keeps_the_failure_tail() {
        let stderr = br#"
phase 01
phase 02
phase 03
phase 04
phase 05
phase 06
phase 07
phase 08
phase 09
phase 10
phase 11
phase 12
phase 13
Error: CHANGELOG.md does not contain a release section
"#;

        let summary = summarize_command_output(b"", stderr).unwrap();

        assert!(!summary.contains("phase 01"));
        assert!(summary.contains("phase 03"));
        assert!(summary.contains("Error: CHANGELOG.md does not contain a release section"));
    }
}
