use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use serde_json::Value;

use crate::infra::process::command_output;
use crate::store::{clean_path, display_path};

const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

fn git_output(
    cwd: &Path,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<Output, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd).args(args);
    command_output(command, GIT_COMMAND_TIMEOUT, "git")
}

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

pub(crate) fn default_worktree_root(repo_root: &Path, env_name: &str) -> PathBuf {
    clean_path(&repo_root.join(".worktrees").join(env_name))
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

    let output = git_output(
        &repo_root,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--detach"),
            worktree_root.as_os_str(),
        ],
    )
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

    let output = git_output(
        worktree_root,
        [
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
        ],
    )
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

    let output = git_output(
        repo_root,
        [
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            worktree_root.as_os_str(),
        ],
    )
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

    let output = git_output(
        worktree_root,
        [
            "-c",
            "status.showUntrackedFiles=all",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )
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
    let worktree_output = git_output(
        worktree_root,
        [
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )
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

    let submodule_output = git_output(
        worktree_root,
        [
            "submodule",
            "foreach",
            "--quiet",
            "--recursive",
            "git ls-files --others --ignored --exclude-standard -z",
        ],
    )
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
    let output = git_output(repo_root, ["worktree", "list", "--porcelain", "-z"])
        .map_err(|error| format!("failed to run git worktree list: {error}"))?;
    if output.status.success() {
        return parse_registered_worktree_paths(&output.stdout);
    }

    let fallback = git_output(
        repo_root,
        [
            "-c",
            "core.quotePath=false",
            "worktree",
            "list",
            "--porcelain",
        ],
    )
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
    let output = git_output(path, ["rev-parse", selector]).ok()?;
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
    use crate::infra::process::command_output;

    use super::{
        ensure_openclaw_worktree, parse_legacy_registered_worktree_paths,
        prepare_openclaw_simulation_worktree_cleanup, remove_openclaw_worktree,
    };

    #[cfg(unix)]
    #[test]
    fn git_timeout_kills_sleep_after_deadline() {
        use std::time::{Duration, Instant};

        let started = Instant::now();
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let error = command_output(command, Duration::from_millis(200), "sleep")
            .expect_err("sleep should be killed at the deadline");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "timed runner should return within 1s, took {elapsed:?}"
        );
        assert!(
            error.contains("timed out"),
            "expected timeout error, got {error}"
        );
    }

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
