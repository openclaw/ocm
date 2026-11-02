use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use super::{UPGRADE_SIMULATION_INTERRUPTED_ERROR, UpgradeInterruptFence};
use crate::openclaw_repo::{detect_openclaw_checkout, git_command};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PullRequestIdentity {
    pub repository: String,
    pub number: u64,
    pub head_sha: String,
    pub url: String,
}

#[derive(Debug)]
pub(super) struct PreparedPullRequest {
    pub identity: PullRequestIdentity,
    checkout: TempDir,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestMetadata {
    number: u64,
    head_ref_oid: String,
}

impl PreparedPullRequest {
    /// Resolve a GitHub PR target into a pinned staging checkout. The run's
    /// interrupt fence is entered into `interrupt_fence` before anything is
    /// staged, so a signal stops resolution at the next gh or Git step and the
    /// checkout is removed on return instead of being left in the temp dir.
    pub fn resolve(
        input: &str,
        env: &BTreeMap<String, String>,
        cwd: &Path,
        interrupt_fence: &mut Option<UpgradeInterruptFence>,
    ) -> Result<Option<Self>, String> {
        let Some((repository, number)) = parse_reference(input)? else {
            return Ok(None);
        };
        let fence = &*interrupt_fence.insert(UpgradeInterruptFence::enter()?);
        let head_sha = step(fence, query_head(&repository, number, env, cwd))?;
        let identity = PullRequestIdentity {
            url: format!("https://github.com/{repository}/pull/{number}"),
            repository,
            number,
            head_sha,
        };
        let checkout = step(
            fence,
            tempfile::Builder::new()
                .prefix("ocm-pr-")
                .tempdir()
                .map_err(|error| format!("failed to prepare GitHub PR checkout: {error}")),
        )?;
        let root = checkout.path();
        step(
            fence,
            checked_git(git_command().arg("-C").arg(root).args(["init", "--quiet"])),
        )?;
        let remote = format!("https://github.com/{}.git", identity.repository);
        step(
            fence,
            checked_git(
                git_command()
                    .arg("-C")
                    .arg(root)
                    .args(["remote", "add", "origin", &remote]),
            ),
        )?;
        step(
            fence,
            checked_git(
                git_command()
                    .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false", "-C"])
                    .arg(root)
                    .args([
                        "fetch",
                        "--no-tags",
                        "--depth=1",
                        "--no-recurse-submodules",
                        "origin",
                    ])
                    .arg(format!("refs/pull/{}/head", identity.number)),
            ),
        )?;
        let fetched = step(
            fence,
            checked_git(
                git_command()
                    .arg("-C")
                    .arg(root)
                    .args(["rev-parse", "FETCH_HEAD"]),
            ),
        )?;
        if fetched.trim() != identity.head_sha
            || step(
                fence,
                query_head(&identity.repository, identity.number, env, cwd),
            )? != identity.head_sha
        {
            return Err(format!(
                "GitHub PR {}#{} changed while resolving; rerun the simulation to select its current head",
                identity.repository, identity.number
            ));
        }
        step(
            fence,
            checked_git(
                git_command()
                    .arg("-C")
                    .arg(root)
                    .args(["checkout", "--quiet", "--detach", "--no-recurse-submodules"])
                    .arg(&identity.head_sha),
            ),
        )?;
        step(
            fence,
            detect_openclaw_checkout(root)
                .ok_or_else(|| "GitHub PR target is not an OpenClaw checkout".to_string()),
        )?;
        Ok(Some(Self { identity, checkout }))
    }

    pub fn materialize(&self, environment_root: &Path) -> Result<PathBuf, String> {
        let owned = tempfile::Builder::new()
            .prefix(".ocm-pr-")
            .tempdir_in(environment_root)
            .map_err(|error| format!("failed to create simulation PR directory: {error}"))?;
        let repo = owned.path().join("source");
        // Each environment owns its objects. Kept simulations and cleanup
        // refusals must remain usable after the run's staging checkout drops.
        checked_git(
            git_command()
                .arg("-C")
                .arg(owned.path())
                .args([
                    "clone",
                    "--quiet",
                    "--no-hardlinks",
                    "--no-recurse-submodules",
                    "--",
                ])
                .arg(self.checkout.path())
                .arg("source"),
        )?;
        checked_git(
            git_command()
                .arg("-C")
                .arg(&repo)
                .args(["remote", "set-url", "origin"])
                .arg(format!(
                    "https://github.com/{}.git",
                    self.identity.repository
                )),
        )?;
        // The existing simulation environment owner now controls cleanup.
        let root = owned.keep();
        Ok(root.join("source"))
    }
}

/// Stop PR resolution once the run's fence has recorded a signal. A Ctrl-C
/// that reached gh or Git also fails that command, and a signal during
/// validation can end in a validation error; both report the interruption.
fn step<T>(fence: &UpgradeInterruptFence, result: Result<T, String>) -> Result<T, String> {
    if fence.requested() {
        Err(UPGRADE_SIMULATION_INTERRUPTED_ERROR.to_string())
    } else {
        result
    }
}

fn query_head(
    repository: &str,
    number: u64,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<String, String> {
    let output = Command::new("gh")
        .args(["pr", "view", &number.to_string(), "--repo", repository])
        .args(["--json", "number,headRefOid"])
        .env_clear()
        .envs(env)
        .env("GH_HOST", "github.com")
        .env("GH_PROMPT_DISABLED", "1")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!(
            "failed to resolve GitHub PR {repository}#{number}: {error}; install/authenticate gh or use a prepared local checkout"
        ))?;
    if !output.status.success() {
        return Err(format!(
            "failed to resolve GitHub PR {repository}#{number}; check gh authentication and PR access, or use a prepared local checkout"
        ));
    }
    let metadata: PullRequestMetadata = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid GitHub PR metadata: {error}"))?;
    if metadata.number != number
        || metadata.head_ref_oid.len() != 40
        || !metadata
            .head_ref_oid
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("GitHub PR metadata has no matching immutable head".to_string());
    }
    Ok(metadata.head_ref_oid.to_ascii_lowercase())
}

fn checked_git(command: &mut Command) -> Result<String, String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("failed to run Git for PR simulation: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "GitHub PR checkout failed: {}; check Git access or use a prepared local checkout",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_reference(input: &str) -> Result<Option<(String, u64)>, String> {
    let input = input.trim();
    let invalid = || {
        "invalid GitHub PR target; use owner/repo#123 or https://github.com/owner/repo/pull/123"
            .to_string()
    };
    let (repository, number) = if input.contains("://") {
        let url = url::Url::parse(input).map_err(|_| invalid())?;
        let parts: Vec<_> = url
            .path()
            .trim_end_matches('/')
            .split('/')
            .skip(1)
            .collect();
        if url.scheme() != "https"
            || url.host_str() != Some("github.com")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || parts.len() != 4
            || parts[2] != "pull"
        {
            return Err(invalid());
        }
        (format!("{}/{}", parts[0], parts[1]), parts[3].to_string())
    } else if let Some((repository, number)) = input.rsplit_once('#') {
        (repository.to_string(), number.to_string())
    } else {
        return Ok(None);
    };
    let parts: Vec<_> = repository.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || matches!(*part, "." | "..")
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
        || number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    let number = number
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(invalid)?;
    Ok(Some((repository, number)))
}

#[cfg(test)]
mod tests {
    use super::parse_reference;

    #[test]
    fn pull_request_references_require_a_github_repository_and_positive_number() {
        for input in [
            "example/openclaw#123",
            "https://github.com/example/openclaw/pull/123/",
        ] {
            assert_eq!(
                parse_reference(input).unwrap(),
                Some(("example/openclaw".into(), 123))
            );
        }
        for input in ["stable", "2026.4.20", "./openclaw"] {
            assert_eq!(parse_reference(input).unwrap(), None);
        }
        for input in [
            "example/openclaw#0",
            "example/openclaw#-1",
            "../openclaw#1",
            "example/repo/extra#1",
            "https://github.com/example/openclaw/issues/123",
            "https://example.com/example/openclaw/pull/123",
            "https://user@github.com/example/openclaw/pull/123",
        ] {
            assert!(parse_reference(input).is_err(), "{input}");
        }
    }
}
