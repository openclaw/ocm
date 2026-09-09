use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use serde_json::Value;

use crate::store::{clean_path, display_path};

const SOURCE_DEPENDENCY_PROBE: &str = r#"import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
if (typeof import.meta.resolve !== "function") {
  throw new Error("Use the Node version supported by the selected OpenClaw checkout.");
}
const requirements = JSON.parse(process.argv[2]);
const localModules = path.join(process.cwd(), "node_modules");
const configuredModules = process.argv[3];
// Match OpenClaw's source loader without creating its startup node_modules link.
const tsxModules = configuredModules && fs.existsSync(path.join(configuredModules, "tsx", "package.json"))
  ? configuredModules : localModules;
const runtimeModules = fs.existsSync(localModules) ? localModules : tsxModules;
const issues = [];
for (const [name, specifier] of requirements) {
  const modules = name === "tsx" ? tsxModules : runtimeModules;
  const manifest = path.join(modules, name, "package.json");
  if (!fs.existsSync(manifest)) {
    issues.push(`${name}: not installed in this checkout`);
    continue;
  }
  try {
    const metadata = JSON.parse(fs.readFileSync(manifest, "utf8"));
    const require = createRequire(manifest);
    let entry;
    if (specifier === "tsx/esm") {
      entry = require.resolve(specifier);
    } else if (modules === localModules) {
      entry = fileURLToPath(import.meta.resolve(specifier));
    } else if (metadata.exports != null) {
      entry = fileURLToPath(import.meta.resolve(specifier, pathToFileURL(manifest)));
    } else {
      // Node's legacy package-main resolver handles packages without exports.
      entry = require.resolve(specifier === name ? "./" : `./${specifier.slice(name.length + 1)}`);
    }
    if (!fs.statSync(entry).isFile()) {
      issues.push(`${specifier}: resolved entry is not a file`);
    }
    if (name === "tsdown") {
      const bin = typeof metadata.bin === "string" ? metadata.bin : metadata.bin?.tsdown;
      if (typeof bin !== "string" || !fs.statSync(path.resolve(path.dirname(manifest), bin)).isFile()) {
        issues.push("tsdown: declared executable is missing");
      }
      const shim = path.join(runtimeModules, ".bin", process.platform === "win32" ? "tsdown.cmd" : "tsdown");
      fs.accessSync(shim, process.platform === "win32" ? fs.constants.F_OK : fs.constants.X_OK);
    }
  } catch (error) {
    issues.push(`${specifier}: ${error.code || "cannot resolve installed entry"}`);
  }
}
process.stdout.write(JSON.stringify(issues));"#;

pub(crate) fn detect_openclaw_checkout(path: &Path) -> Option<PathBuf> {
    let package_json = path.join("package.json");
    let scripts_dir = path.join("scripts");
    if !package_json.exists() || !scripts_dir.join("run-node.mjs").exists() {
        return None;
    }

    let contents = fs::read_to_string(package_json).ok()?;
    let package: Value = serde_json::from_str(&contents).ok()?;
    if package.get("name").and_then(Value::as_str) == Some("openclaw") {
        Some(clean_path(path))
    } else {
        None
    }
}

pub(crate) fn discover_openclaw_checkout(cwd: &Path) -> Option<PathBuf> {
    for ancestor in cwd.ancestors().take(8) {
        if let Some(checkout) = detect_openclaw_checkout(ancestor) {
            return Some(checkout);
        }

        let sibling = ancestor.join("openclaw");
        if let Some(checkout) = detect_openclaw_checkout(&sibling) {
            return Some(checkout);
        }
    }

    None
}

pub(crate) fn discover_enclosing_openclaw_checkout(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find_map(detect_openclaw_checkout)
}

/// Rejects checkout dependencies that resolve through another checkout.
pub(crate) fn ensure_checkout_owned_dependencies(repo_root: &Path) -> Result<(), String> {
    let node_modules = repo_root.join("node_modules");
    match fs::symlink_metadata(&node_modules) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "failed to inspect OpenClaw dependencies at {}: {error}",
                display_path(&node_modules)
            ));
        }
    }

    // Compare resolved directories so relative links from linked worktrees cannot
    // make another checkout's dependency tree appear local.
    let resolved_repo = fs::canonicalize(repo_root).map_err(|error| {
        format!(
            "failed to resolve selected OpenClaw checkout at {}: {error}",
            display_path(repo_root)
        )
    })?;
    let resolved_dependencies = fs::canonicalize(&node_modules).map_err(|error| {
        format!(
            "failed to resolve OpenClaw dependencies at {}: {error}",
            display_path(&node_modules)
        )
    })?;
    if resolved_dependencies.starts_with(&resolved_repo) {
        return Ok(());
    }

    // OCM leaves the selected checkout untouched. A standalone checkout gives
    // package managers an isolated dependency tree without rewriting the worktree.
    Err(format!(
        "OpenClaw dependencies resolve outside the selected checkout: {} -> {}. OCM will not build or run source against dependencies from another checkout. Use a standalone checkout at the selected commit, run `pnpm install --frozen-lockfile` there, and pass that checkout with `--repo`.",
        display_path(&node_modules),
        display_path(&resolved_dependencies)
    ))
}

pub(crate) fn inspect_source_dependencies(
    repo_root: &Path,
    env: &BTreeMap<String, String>,
    watch: bool,
) -> Result<Option<String>, String> {
    ensure_checkout_owned_dependencies(repo_root)?;
    for script in ["scripts/run-node.mjs"]
        .into_iter()
        .chain(watch.then_some("scripts/watch-node.mjs"))
    {
        if !repo_root.join(script).is_file() {
            return Err(format!(
                "OpenClaw source entry is missing: {}",
                display_path(&repo_root.join(script))
            ));
        }
    }
    let manifest_path = repo_root.join("package.json");
    let contents = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "failed reading OpenClaw package metadata {}: {error}",
            display_path(&manifest_path)
        )
    })?;
    let manifest: Value = serde_json::from_str(&contents).map_err(|error| {
        format!(
            "invalid OpenClaw package metadata {}: {error}",
            display_path(&manifest_path)
        )
    })?;
    for section in ["dependencies", "devDependencies", "optionalDependencies"] {
        if let Some(value) = manifest.get(section)
            && !value.is_object()
            && !value.is_null()
        {
            return Err(format!(
                "invalid OpenClaw package metadata: {section} must be an object"
            ));
        }
    }
    // These are the source runner's build tools and the watch runner's watcher,
    // not a completeness check for the application's runtime dependency graph.
    let declares = |name: &str| {
        ["dependencies", "devDependencies", "optionalDependencies"]
            .into_iter()
            .any(|section| {
                manifest
                    .get(section)
                    .and_then(Value::as_object)
                    .is_some_and(|dependencies| dependencies.contains_key(name))
            })
    };
    let mut requirements = Vec::new();
    if declares("tsx") {
        requirements.push(("tsx", "tsx"));
        if repo_root.join("scripts/tsx.mjs").is_file() {
            requirements.push(("tsx", "tsx/esm"));
        }
    }
    if declares("tsdown") {
        requirements.push(("tsdown", "tsdown"));
    }
    if watch && declares("chokidar") {
        requirements.push(("chokidar", "chokidar"));
    }
    if requirements.is_empty() {
        return Ok(None);
    }
    let modules_override = source_modules_override(repo_root, env)?
        .as_deref()
        .map(display_path)
        .unwrap_or_default();
    let requirements = serde_json::to_string(&requirements).map_err(|error| error.to_string())?;
    let output = Command::new("node")
        .args([
            "--input-type=module",
            "--experimental-import-meta-resolve",
            "--eval",
            SOURCE_DEPENDENCY_PROBE,
            "--",
            "ocm-source-dependencies",
            &requirements,
            &modules_override,
        ])
        .env_clear()
        .envs(env)
        // Validation resolves installed files without running user preload hooks.
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH")
        .env_remove("NODE_COMPILE_CACHE")
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| {
            format!("failed to run node for OpenClaw source prerequisite checks: {error}")
        })?;
    if !output.status.success() {
        return Err(format!(
            "OpenClaw source prerequisite check failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let issues: Vec<String> = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid OpenClaw source prerequisite result: {error}"))?;
    Ok((!issues.is_empty()).then(|| issues.join("; ")))
}

fn source_modules_override(
    repo_root: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Option<PathBuf>, String> {
    let configured = env
        .get_key_value("PNPM_CONFIG_MODULES_DIR")
        .or_else(|| env.get_key_value("pnpm_config_modules_dir"))
        .filter(|(_, value)| !value.is_empty())
        .or_else(|| {
            env.get_key_value("npm_config_modules_dir")
                .filter(|(_, value)| !value.is_empty())
        });
    let Some((key, value)) = configured else {
        return Ok(None);
    };
    let configured = clean_path(&repo_root.join(value));
    let resolved_repo = fs::canonicalize(repo_root).map_err(|error| error.to_string())?;
    // An absent install target is safe only when its nearest existing ancestor
    // belongs to this checkout. Do not skip broken links while finding it.
    let mut existing = configured.as_path();
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                existing = existing.parent().ok_or_else(|| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let resolved = fs::canonicalize(existing).map_err(|error| {
        format!("failed resolving OpenClaw source dependency override {key}: {error}")
    })?;
    if !resolved.starts_with(&resolved_repo) {
        return Err(format!(
            "OpenClaw source dependency override {key} resolves outside the selected checkout: {}; unset it or select dependencies inside {}",
            display_path(&configured),
            display_path(repo_root)
        ));
    }
    Ok(Some(configured))
}

pub(crate) fn ensure_source_dependency_install_target(
    repo_root: &Path,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    ensure_checkout_owned_dependencies(repo_root)?;
    let mut modules = vec![repo_root.join("node_modules")];
    if let Some(configured) = source_modules_override(repo_root, env)?
        && !modules.contains(&configured)
    {
        modules.push(configured);
    }
    for directory in modules
        .into_iter()
        .flat_map(|root| [root.clone(), root.join(".pnpm")])
    {
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(format!(
                    "refusing to install OpenClaw source dependencies through {}; preserve the linked or invalid dependency tree and prepare it explicitly",
                    display_path(&directory)
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "failed inspecting OpenClaw dependency install target {}: {error}",
                    display_path(&directory)
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn default_worktree_root(repo_root: &Path, env_name: &str) -> PathBuf {
    clean_path(&repo_root.join(".worktrees").join(env_name))
}

pub(crate) fn validate_openclaw_worktree(
    repo_root: &Path,
    worktree_root: &Path,
) -> Result<(), String> {
    if !worktree_root.exists() {
        return Err(format!(
            "saved dev worktree is missing: {}; restore that checkout before resuming the env",
            display_path(worktree_root)
        ));
    }
    let registered = registered_worktree_paths(repo_root)?;
    if !contains_worktree_path(&registered, worktree_root) {
        return Err(format!(
            "saved dev worktree is not registered to this OpenClaw checkout: {}",
            display_path(worktree_root)
        ));
    }
    if !is_existing_openclaw_worktree(repo_root, worktree_root) {
        return Err(format!(
            "registered worktree is not a valid OpenClaw checkout: {}",
            display_path(worktree_root)
        ));
    }
    Ok(())
}

pub(crate) fn ensure_openclaw_worktree(
    repo_root: &Path,
    env_name: &str,
) -> Result<PathBuf, String> {
    let repo_root = detect_openclaw_checkout(repo_root)
        .ok_or_else(|| format!("OpenClaw checkout not found at {}", display_path(repo_root)))?;
    let worktree_root = default_worktree_root(&repo_root, env_name);
    let registered = registered_worktree_paths(&repo_root)?;
    let worktree_registered = contains_worktree_path(&registered, &worktree_root);

    if worktree_registered {
        if !worktree_root.exists() {
            remove_registered_worktree(&repo_root, &worktree_root)?;
        } else if is_existing_openclaw_worktree(&repo_root, &worktree_root) {
            return Ok(worktree_root);
        } else {
            return Err(format!(
                "registered worktree is not a valid OpenClaw checkout: {}",
                display_path(&worktree_root)
            ));
        }
    }

    if worktree_root.exists() {
        return Err(format!(
            "worktree path already exists but is not registered to this OpenClaw checkout: {}",
            display_path(&worktree_root)
        ));
    }

    if let Some(parent) = worktree_root.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["worktree", "add", "--detach"])
        .arg(&worktree_root)
        .output()
        .map_err(|error| format!("failed to run git worktree add: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(format!("git worktree add failed: {detail}"));
    }

    let registered = registered_worktree_paths(&repo_root)?;
    if !contains_worktree_path(&registered, &worktree_root)
        || !is_existing_openclaw_worktree(&repo_root, &worktree_root)
    {
        return Err(format!(
            "created worktree is not a valid OpenClaw checkout: {}",
            display_path(&worktree_root)
        ));
    }

    Ok(worktree_root)
}

pub(crate) fn remove_openclaw_worktree(
    repo_root: &Path,
    worktree_root: &Path,
) -> Result<(), String> {
    remove_openclaw_worktree_checked(repo_root, worktree_root)
}

pub(crate) fn prepare_openclaw_simulation_worktree_cleanup(
    repo_root: &Path,
    worktree_root: &Path,
    simulation_name: &str,
) -> Result<(), String> {
    let expected = default_worktree_root(repo_root, simulation_name);
    if normalize_worktree_path(worktree_root) != normalize_worktree_path(&expected) {
        return Err(format!(
            "refusing simulation cleanup outside its OCM-owned worktree: expected {}, found {}",
            display_path(&expected),
            display_path(worktree_root)
        ));
    }

    ensure_registered_worktree_identity(repo_root, worktree_root)?;
    remove_generated_simulation_outputs(worktree_root)
}

fn remove_openclaw_worktree_checked(repo_root: &Path, worktree_root: &Path) -> Result<(), String> {
    if !ensure_registered_worktree_identity(repo_root, worktree_root)? {
        return Ok(());
    }
    remove_registered_worktree(repo_root, worktree_root)
}

fn ensure_registered_worktree_identity(
    repo_root: &Path,
    worktree_root: &Path,
) -> Result<bool, String> {
    let registered = match registered_worktree_paths(repo_root) {
        Ok(registered) => registered,
        Err(_) if !worktree_root.exists() => return Ok(false),
        Err(error) => return Err(error),
    };
    if !contains_worktree_path(&registered, worktree_root) {
        if worktree_root.exists() {
            return Err(format!(
                "refusing to remove worktree path not registered to this OpenClaw checkout: {}",
                display_path(worktree_root)
            ));
        }
        return Ok(false);
    }

    if worktree_root.exists() && !has_expected_worktree_identity(repo_root, worktree_root) {
        return Err(format!(
            "refusing to remove registered worktree whose checkout identity does not match this OpenClaw checkout: {}",
            display_path(worktree_root)
        ));
    }

    Ok(true)
}

fn remove_generated_simulation_outputs(worktree_root: &Path) -> Result<(), String> {
    if !worktree_root.exists() {
        return Ok(());
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(worktree_root)
        .args([
            "clean",
            "-ffdX",
            "--",
            "node_modules",
            ":(glob)**/node_modules",
            ".artifacts",
            "dist",
            "dist-runtime",
            ":(glob)packages/*/dist",
            "extensions/canvas/src/host/a2ui",
            "extensions/diffs-language-pack/assets",
            "extensions/diffs/assets",
            "extensions/discord/assets",
        ])
        .output()
        .map_err(|error| format!("failed to remove generated simulation output: {error}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    Err(format!(
        "failed to remove generated simulation output: {detail}"
    ))
}

fn remove_registered_worktree(repo_root: &Path, worktree_root: &Path) -> Result<(), String> {
    ensure_worktree_clean(worktree_root)?;

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(worktree_root)
        .output()
        .map_err(|error| format!("failed to run git worktree remove: {error}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    Err(format!("git worktree remove failed: {detail}"))
}

fn ensure_worktree_clean(worktree_root: &Path) -> Result<(), String> {
    if !worktree_root.exists() {
        return Ok(());
    }

    let output = Command::new("git")
        .args(["-c", "status.showUntrackedFiles=all"])
        .arg("-C")
        .arg(worktree_root)
        .args([
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ])
        .output()
        .map_err(|error| format!("failed to inspect git worktree status: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(format!("git worktree status failed: {detail}"));
    }
    if !output.stdout.is_empty() {
        return Err(format!(
            "git worktree remove failed: {} contains modified or untracked files",
            display_path(worktree_root)
        ));
    }

    ensure_no_ignored_local_files(worktree_root)?;
    Ok(())
}

fn ensure_no_ignored_local_files(worktree_root: &Path) -> Result<(), String> {
    let worktree_output = Command::new("git")
        .arg("-C")
        .arg(worktree_root)
        .args([
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ])
        .output()
        .map_err(|error| format!("failed to inspect ignored worktree files: {error}"))?;
    if !worktree_output.status.success() {
        let stderr = String::from_utf8_lossy(&worktree_output.stderr)
            .trim()
            .to_string();
        let stdout = String::from_utf8_lossy(&worktree_output.stdout)
            .trim()
            .to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(format!("git ignored-file inspection failed: {detail}"));
    }

    let submodule_output = Command::new("git")
        .arg("-C")
        .arg(worktree_root)
        .args([
            "submodule",
            "foreach",
            "--quiet",
            "--recursive",
            "git ls-files --others --ignored --exclude-standard -z",
        ])
        .output()
        .map_err(|error| format!("failed to inspect ignored submodule files: {error}"))?;
    if !submodule_output.status.success() {
        let stderr = String::from_utf8_lossy(&submodule_output.stderr)
            .trim()
            .to_string();
        let stdout = String::from_utf8_lossy(&submodule_output.stdout)
            .trim()
            .to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(format!(
            "git ignored-file inspection failed for initialized submodules: {detail}"
        ));
    }

    let has_local_files = [&worktree_output.stdout, &submodule_output.stdout]
        .into_iter()
        .flat_map(|output| output.split(|byte| *byte == 0))
        .filter(|path| !path.is_empty())
        .map(git_path_from_bytes)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(PathBuf::from)
        .any(|path| !is_disposable_ignored_path(&path));
    if has_local_files {
        return Err(format!(
            "git worktree remove failed: {} contains ignored local files",
            display_path(worktree_root)
        ));
    }

    Ok(())
}

fn is_disposable_ignored_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "node_modules")
}

fn registered_worktree_paths(repo_root: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .map_err(|error| format!("failed to run git worktree list: {error}"))?;
    if output.status.success() {
        return parse_registered_worktree_paths(&output.stdout);
    }

    let fallback = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "-c",
            "core.quotePath=false",
            "worktree",
            "list",
            "--porcelain",
        ])
        .output()
        .map_err(|error| format!("failed to run compatible git worktree list: {error}"))?;
    if fallback.status.success() {
        return parse_legacy_registered_worktree_paths(&fallback.stdout);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    let fallback_stderr = String::from_utf8_lossy(&fallback.stderr).trim().to_string();
    let fallback_stdout = String::from_utf8_lossy(&fallback.stdout).trim().to_string();
    let fallback_detail = if !fallback_stderr.is_empty() {
        fallback_stderr
    } else {
        fallback_stdout
    };
    Err(format!(
        "git worktree list failed: {detail}; compatible fallback failed: {fallback_detail}"
    ))
}

fn parse_registered_worktree_paths(output: &[u8]) -> Result<Vec<PathBuf>, String> {
    output
        .split(|byte| *byte == 0)
        .filter_map(|field| field.strip_prefix(b"worktree "))
        .map(|path| git_path_from_bytes(path).map(PathBuf::from))
        .collect()
}

fn parse_legacy_registered_worktree_paths(output: &[u8]) -> Result<Vec<PathBuf>, String> {
    output
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.strip_prefix(b"worktree "))
        .map(|path| {
            if path.starts_with(b"\"") {
                return Err(
                    "git worktree list returned a quoted path that requires Git 2.36 or newer"
                        .to_string(),
                );
            }
            git_path_from_bytes(path).map(PathBuf::from)
        })
        .collect()
}

fn contains_worktree_path(registered: &[PathBuf], expected: &Path) -> bool {
    let expected = normalize_worktree_path(expected);
    registered
        .iter()
        .any(|path| normalize_worktree_path(path) == expected)
}

fn normalize_worktree_path(path: &Path) -> PathBuf {
    let Some(parent) = path.parent() else {
        return clean_path(path);
    };
    let Some(name) = path.file_name() else {
        return clean_path(path);
    };

    let mut ancestor = parent;
    let mut missing = Vec::<OsString>::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            return clean_path(path);
        };
        missing.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return clean_path(path);
        };
        ancestor = parent;
    }

    let mut normalized = fs::canonicalize(ancestor).unwrap_or_else(|_| clean_path(ancestor));
    for component in missing.into_iter().rev() {
        normalized.push(component);
    }
    normalized.push(name);
    clean_path(&normalized)
}

fn is_existing_openclaw_worktree(repo_root: &Path, path: &Path) -> bool {
    detect_openclaw_checkout(path).is_some() && has_expected_worktree_identity(repo_root, path)
}

fn has_expected_worktree_identity(repo_root: &Path, path: &Path) -> bool {
    path.exists()
        && path.join(".git").exists()
        && git_top_level(path).is_some_and(|top_level| {
            normalize_worktree_path(&top_level) == normalize_worktree_path(path)
        })
        && git_common_dir(repo_root)
            .zip(git_common_dir(path))
            .is_some_and(|(repo, worktree)| repo == worktree)
        && git_worktree_backlink(path).is_some_and(|backlink| {
            normalize_git_file_path(&backlink) == normalize_git_file_path(&path.join(".git"))
        })
}

fn git_common_dir(path: &Path) -> Option<PathBuf> {
    git_rev_parse_path(path, "--git-common-dir")
}

fn git_top_level(path: &Path) -> Option<PathBuf> {
    git_rev_parse_path(path, "--show-toplevel")
}

fn git_worktree_backlink(path: &Path) -> Option<PathBuf> {
    let git_dir = git_rev_parse_path(path, "--git-dir")?;
    let backlink = fs::read(git_dir.join("gitdir")).ok()?;
    let backlink = trim_git_line(&backlink);
    let backlink = PathBuf::from(git_path_from_bytes(backlink).ok()?);
    if backlink.is_absolute() {
        Some(backlink)
    } else {
        Some(clean_path(&git_dir.join(backlink)))
    }
}

fn git_rev_parse_path(path: &Path, selector: &str) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", selector])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let resolved = PathBuf::from(git_path_from_bytes(trim_git_line(&output.stdout)).ok()?);
    let resolved = if resolved.is_absolute() {
        resolved
    } else {
        clean_path(&path.join(resolved))
    };
    fs::canonicalize(&resolved)
        .ok()
        .or_else(|| Some(clean_path(&resolved)))
}

fn trim_git_line(output: &[u8]) -> &[u8] {
    let output = output.strip_suffix(b"\n").unwrap_or(output);
    output.strip_suffix(b"\r").unwrap_or(output)
}

fn normalize_git_file_path(path: &Path) -> PathBuf {
    let Some(parent) = path.parent() else {
        return clean_path(path);
    };
    let Some(name) = path.file_name() else {
        return clean_path(path);
    };
    let parent = fs::canonicalize(parent).unwrap_or_else(|_| clean_path(parent));
    clean_path(&parent.join(name))
}

#[cfg(unix)]
fn git_path_from_bytes(path: &[u8]) -> Result<OsString, String> {
    Ok(OsString::from_vec(path.to_vec()))
}

#[cfg(not(unix))]
fn git_path_from_bytes(path: &[u8]) -> Result<OsString, String> {
    String::from_utf8(path.to_vec())
        .map(OsString::from)
        .map_err(|_| "git returned a non-UTF-8 path".to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    use tempfile::TempDir;

    #[cfg(unix)]
    use super::parse_registered_worktree_paths;
    use super::{
        ensure_openclaw_worktree, parse_legacy_registered_worktree_paths,
        prepare_openclaw_simulation_worktree_cleanup, remove_openclaw_worktree,
    };

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_openclaw_repo() -> (TempDir, PathBuf) {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("openclaw");
        fs::create_dir_all(repo.join("scripts")).unwrap();
        fs::write(
            repo.join("package.json"),
            r#"{"name":"openclaw","version":"test"}"#,
        )
        .unwrap();
        fs::write(repo.join("scripts/run-node.mjs"), "console.log('test');\n").unwrap();
        fs::write(
            repo.join(".gitignore"),
            "node_modules/\n.artifacts/\ndist/\ndist-runtime/\npackages/*/dist/\nextensions/canvas/src/host/a2ui/\nextensions/diffs-language-pack/assets/\nextensions/diffs/assets/\nextensions/discord/assets/\n.env\n",
        )
        .unwrap();

        let init = Command::new("git").arg("init").arg(&repo).output().unwrap();
        assert!(init.status.success());
        run_git(&repo, &["config", "user.email", "tests@example.com"]);
        run_git(&repo, &["config", "user.name", "OCM Tests"]);
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-m", "init"]);
        (temp, repo)
    }

    #[test]
    fn simulation_cleanup_discards_ignored_outputs_only_for_owned_worktree() {
        let (_temp, repo) = init_openclaw_repo();
        let worktree = ensure_openclaw_worktree(&repo, "demo-sim").unwrap();
        for relative in [
            "node_modules/pkg/index.js",
            "extensions/demo/node_modules/pkg/index.js",
            ".artifacts/build.json",
            "dist/index.js",
            "dist-runtime/index.js",
            "packages/demo/dist/index.js",
            "extensions/canvas/src/host/a2ui/index.html",
            "extensions/diffs-language-pack/assets/viewer-runtime.js",
            "extensions/diffs/assets/viewer-runtime.js",
            "extensions/discord/assets/activity.js",
        ] {
            let path = worktree.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "generated\n").unwrap();
        }

        let ordinary_error = remove_openclaw_worktree(&repo, &worktree).unwrap_err();
        assert!(ordinary_error.contains("contains ignored local files"));
        assert!(worktree.exists());

        let ownership_error =
            prepare_openclaw_simulation_worktree_cleanup(&repo, &worktree, "other-sim")
                .unwrap_err();
        assert!(ownership_error.contains("outside its OCM-owned worktree"));
        assert!(worktree.exists());

        prepare_openclaw_simulation_worktree_cleanup(&repo, &worktree, "demo-sim").unwrap();
        assert!(worktree.exists());
        remove_openclaw_worktree(&repo, &worktree).unwrap();
        assert!(!worktree.exists());
    }

    #[test]
    fn simulation_cleanup_preserves_untracked_files() {
        let (_temp, repo) = init_openclaw_repo();
        let worktree = ensure_openclaw_worktree(&repo, "demo-sim").unwrap();
        fs::write(worktree.join("operator-notes.txt"), "preserve\n").unwrap();

        prepare_openclaw_simulation_worktree_cleanup(&repo, &worktree, "demo-sim").unwrap();
        let error = remove_openclaw_worktree(&repo, &worktree).unwrap_err();
        assert!(error.contains("contains modified or untracked files"));
        assert_eq!(
            fs::read_to_string(worktree.join("operator-notes.txt")).unwrap(),
            "preserve\n"
        );
    }

    #[test]
    fn simulation_cleanup_preserves_non_build_ignored_files() {
        let (_temp, repo) = init_openclaw_repo();
        let worktree = ensure_openclaw_worktree(&repo, "demo-sim").unwrap();
        fs::write(worktree.join(".env"), "PRIVATE_VALUE=preserve\n").unwrap();
        let generated = worktree.join("dist/index.js");
        fs::create_dir_all(generated.parent().unwrap()).unwrap();
        fs::write(&generated, "generated\n").unwrap();

        prepare_openclaw_simulation_worktree_cleanup(&repo, &worktree, "demo-sim").unwrap();
        let error = remove_openclaw_worktree(&repo, &worktree).unwrap_err();
        assert!(error.contains("contains ignored local files"));
        assert_eq!(
            fs::read_to_string(worktree.join(".env")).unwrap(),
            "PRIVATE_VALUE=preserve\n"
        );
        assert!(!generated.exists());
        assert!(worktree.exists());
    }

    #[test]
    fn simulation_cleanup_does_not_touch_replaced_unregistered_checkout() {
        let (_temp, repo) = init_openclaw_repo();
        let worktree = ensure_openclaw_worktree(&repo, "demo-sim").unwrap();
        run_git(
            &repo,
            &[
                "worktree",
                "remove",
                "--force",
                "--",
                worktree.to_str().unwrap(),
            ],
        );

        fs::create_dir_all(worktree.join("dist")).unwrap();
        fs::write(worktree.join(".gitignore"), "dist/\n").unwrap();
        fs::write(worktree.join("dist/operator-output.txt"), "preserve\n").unwrap();
        let init = Command::new("git")
            .arg("init")
            .arg(&worktree)
            .output()
            .unwrap();
        assert!(init.status.success());

        let error =
            prepare_openclaw_simulation_worktree_cleanup(&repo, &worktree, "demo-sim").unwrap_err();
        assert!(error.contains("not registered"));
        assert_eq!(
            fs::read_to_string(worktree.join("dist/operator-output.txt")).unwrap(),
            "preserve\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn worktree_porcelain_parser_preserves_non_utf8_paths() {
        let paths = parse_registered_worktree_paths(
            b"worktree /tmp/openclaw\0HEAD abc\0\0worktree /tmp/other\xff\0HEAD def\0\0",
        )
        .unwrap();

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[1].as_os_str().as_bytes(), b"/tmp/other\xff");
    }

    #[test]
    fn legacy_worktree_porcelain_parser_preserves_spaces() {
        let paths = parse_legacy_registered_worktree_paths(
            b"worktree /tmp/openclaw checkout\nHEAD abc123\ndetached\n\n",
        )
        .unwrap();

        assert_eq!(paths, vec![PathBuf::from("/tmp/openclaw checkout")]);
    }

    #[test]
    fn legacy_worktree_porcelain_parser_rejects_quoted_paths() {
        let error = parse_legacy_registered_worktree_paths(
            br#"worktree "/tmp/openclaw\ncheckout"
HEAD abc123
detached

"#,
        )
        .unwrap_err();

        assert!(error.contains("requires Git 2.36 or newer"));
    }
}
