use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::Value;

use super::Cli;
use crate::launcher::{LauncherMeta, parse_literal_launcher_command};
use crate::openclaw_repo::git_command;
use crate::store::display_path;

/// Observations only: matching commits do not prove build completion or readiness.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceInspection {
    pub root: String,
    pub head: Option<String>,
    pub built_commit: Option<String>,
    pub built_version: Option<String>,
    pub build_matches_head: Option<bool>,
    pub working_tree_clean: Option<bool>,
    pub tracking_ref: Option<String>,
    pub tracking_head: Option<String>,
    pub shared_environments: Vec<String>,
    pub issues: Vec<String>,
}

impl Cli {
    pub(super) fn inspect_launcher_source(
        &self,
        env_name: &str,
        launcher_name: &str,
    ) -> Result<Option<SourceInspection>, String> {
        let launcher = self.launcher_service().show(launcher_name)?;
        let Some(root) = launcher_source_root(&launcher) else {
            return Ok(None);
        };
        let mut source = SourceInspection {
            root: display_path(&root),
            head: None,
            built_commit: None,
            built_version: None,
            build_matches_head: None,
            working_tree_clean: None,
            tracking_ref: None,
            tracking_head: None,
            shared_environments: Vec::new(),
            issues: Vec::new(),
        };
        // A package nested inside an unrelated repository is not a source checkout.
        let top_level = inspect_git(&root, &["rev-parse", "--show-toplevel"])
            .and_then(|path| fs::canonicalize(path.trim()).ok());
        if top_level.as_ref() == Some(&root) {
            // These fixed ref queries do not read object contents. Object-reading
            // commands must stay behind inspect_working_tree's promisor guard.
            source.head = inspect_git(&root, &["rev-parse", "--verify", "HEAD"])
                .and_then(|value| full_commit(value.trim()));
            source.working_tree_clean = inspect_working_tree(&root, &mut source.issues);
            source.tracking_ref =
                inspect_git(&root, &["rev-parse", "--symbolic-full-name", "@{upstream}"])
                    .map(|value| value.trim().to_string());
            source.tracking_head = inspect_git(&root, &["rev-parse", "--verify", "@{upstream}"])
                .and_then(|value| full_commit(value.trim()));
        }
        if source.head.is_none() || source.working_tree_clean.is_none() {
            source
                .issues
                .push("Git checkout identity or working-tree status could not be read".to_string());
        }
        if source.working_tree_clean == Some(false) {
            source
                .issues
                .push("checkout has modified or untracked files".to_string());
        }
        match read_source_json(&root, "dist/build-info.json", 65536) {
            Ok(info) => {
                source.built_commit = info
                    .get("commit")
                    .and_then(Value::as_str)
                    .and_then(full_commit);
                source.built_version = info
                    .get("version")
                    .and_then(Value::as_str)
                    .filter(|value| {
                        !value.is_empty() && value.len() <= 128 && !value.contains(char::is_control)
                    })
                    .map(str::to_string);
                if source.built_commit.is_none() {
                    source
                        .issues
                        .push("build metadata has no full built commit".to_string());
                }
            }
            Err(issue) => source.issues.push(issue),
        }
        source.build_matches_head = source
            .head
            .as_ref()
            .zip(source.built_commit.as_ref())
            .map(|(head, built)| head == built);
        if source.build_matches_head == Some(false) {
            source.issues.push(
                "checkout HEAD differs from the built artifact; pulling source does not rebuild it"
                    .to_string(),
            );
        }
        // Report known registered consumers, without claiming to exclude unmanaged processes.
        for other in self.environment_service().list()? {
            if other.name == env_name {
                continue;
            }
            let mut paths = Vec::new();
            if let Some(dev) = other.dev.as_ref() {
                paths.push(PathBuf::from(dev.source_root()));
            }
            if let Some(name) = other.default_launcher.as_deref() {
                match self.launcher_service().show(name) {
                    Ok(launcher) => {
                        if let Some(path) = launcher_source_root(&launcher) {
                            paths.push(path);
                        }
                        if let Some(cwd) = launcher.cwd {
                            paths.push(PathBuf::from(cwd));
                        }
                    }
                    Err(_) => source.issues.push(format!(
                        "could not inspect launcher binding for environment {}",
                        other.name
                    )),
                }
            }
            if let Some(name) = other.default_runtime.as_deref() {
                match self.runtime_service().show(name) {
                    Ok(runtime) => paths.push(PathBuf::from(runtime.binary_path)),
                    Err(_) => source.issues.push(format!(
                        "could not inspect runtime binding for environment {}",
                        other.name
                    )),
                }
            }
            if paths
                .iter()
                .filter_map(|path| fs::canonicalize(path).ok())
                .any(|path| path.starts_with(&root) || root.starts_with(path))
            {
                source.shared_environments.push(other.name);
            }
        }
        if !source.shared_environments.is_empty() {
            source
                .issues
                .push("other registered environments refer to this source path".to_string());
        }
        Ok(Some(source))
    }
}

pub(super) fn launcher_source_root(launcher: &LauncherMeta) -> Option<PathBuf> {
    let cwd = launcher.cwd.as_deref().map(Path::new);
    let words = parse_literal_launcher_command(&launcher.command)?;
    let (program, args) = words.split_first()?;
    let program = Path::new(program).file_name()?.to_str()?;
    let root = match (program, args) {
        ("pnpm" | "pnpm.cmd", [script]) if script == "openclaw" => cwd?.to_path_buf(),
        ("node" | "node.exe", [entry]) => {
            let entry = Path::new(entry);
            let entry = if entry.is_absolute() {
                entry.to_path_buf()
            } else {
                cwd?.join(entry)
            };
            let entry = fs::canonicalize(entry).ok()?;
            if entry.file_name()? != "openclaw.mjs" {
                return None;
            }
            entry.parent()?.to_path_buf()
        }
        _ => return None,
    };
    let root = fs::canonicalize(root).ok()?;
    let package = read_source_json(&root, "package.json", 1024 * 1024).ok()?;
    if package.get("name")?.as_str()? != "openclaw" {
        return None;
    }
    if matches!(program, "pnpm" | "pnpm.cmd")
        && package.pointer("/scripts/openclaw")?.as_str()? != "node scripts/run-node.mjs"
    {
        return None;
    }
    let runner = fs::canonicalize(root.join("scripts/run-node.mjs")).ok()?;
    (runner.starts_with(&root) && runner.is_file()).then_some(root)
}

fn inspection_git_command(root: &Path) -> Command {
    let mut command = git_command();
    command
        .env("GIT_NO_LAZY_FETCH", "1")
        // GIT_CONFIG redirects only `git config`, unlike the commands it guards.
        .env_remove("GIT_CONFIG")
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-C",
        ])
        .arg(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn has_no_promisor_configuration(root: &Path) -> bool {
    // Config inspection cannot load repository objects. Include filter-only remote
    // configuration because older Git also treats that as a promisor remote.
    inspection_git_command(root)
        .args([
            "config",
            "--name-only",
            "--get-regexp",
            r"^(extensions\.partialclone|remote\..*\.(promisor|partialclonefilter))$",
        ])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|status| status.code() == Some(1))
}

fn inspect_git(root: &Path, args: &[&str]) -> Option<String> {
    let output = inspection_git_command(root).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn inspect_working_tree(root: &Path, issues: &mut Vec<String>) -> Option<bool> {
    // Older Git ignores GIT_NO_LAZY_FETCH, and even --no-lazy-fetch does not
    // suppress every rename prefetch path. Avoid object reads in partial clones.
    if !has_no_promisor_configuration(root) {
        issues.push(
            "working-tree inspection skipped because Git cannot rule out fetching missing objects"
                .to_string(),
        );
        return None;
    }
    // Status may execute a configured clean/process filter, even without index writes.
    let filters = inspection_git_command(root)
        .args([
            "config",
            "--name-only",
            "--get-regexp",
            r"^filter\..*\.(clean|process)$",
        ])
        .output()
        .ok()?;
    match filters.status.code() {
        Some(1) => {} // No matching configuration.
        Some(0) => {
            issues.push(
                "working-tree inspection skipped because Git filters can execute commands"
                    .to_string(),
            );
            return None;
        }
        _ => return None,
    }
    // Nested repositories have their own executable configuration.
    let entries = inspect_git(root, &["ls-files", "--stage"])?;
    if entries.lines().any(|line| line.starts_with("160000 ")) {
        issues.push(
            "working-tree inspection skipped because the checkout contains submodules".to_string(),
        );
        return None;
    }
    inspect_git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            // Keep staged gitlink removals/type changes visible without scanning
            // nested worktrees. Any gitlink still in the index was rejected above.
            "--ignore-submodules=dirty",
        ],
    )
    .map(|status| status.is_empty())
}

fn full_commit(value: &str) -> Option<String> {
    ((value.len() == 40 || value.len() == 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

pub(super) fn read_source_json(root: &Path, relative: &str, limit: u64) -> Result<Value, String> {
    let path = fs::canonicalize(root.join(relative))
        .map_err(|_| format!("{relative} is missing or unreadable"))?;
    if !path.starts_with(root) {
        return Err(format!("{relative} resolves outside the checkout"));
    }
    if !path.is_file() {
        return Err(format!("{relative} is not a regular file"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|_| format!("{relative} could not be read"))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{relative} is too large"));
    }
    serde_json::from_slice(&bytes).map_err(|_| format!("{relative} is invalid JSON"))
}
