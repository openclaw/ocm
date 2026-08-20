mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::support::{TestDir, path_string, write_executable_script};

struct ReleaseRepo {
    _root: TestDir,
    repo: PathBuf,
    remote: PathBuf,
    home: PathBuf,
    env_path: String,
    git: String,
    ghx: PathBuf,
}

impl ReleaseRepo {
    fn apply_toolchain_env(&self, command: &mut Command) {
        if let Some(value) = std::env::var_os("RUSTUP_TOOLCHAIN") {
            command.env("RUSTUP_TOOLCHAIN", value);
        }
        let host_home = PathBuf::from(std::env::var_os("HOME").unwrap());
        command.env(
            "CARGO_HOME",
            std::env::var_os("CARGO_HOME").unwrap_or_else(|| host_home.join(".cargo").into()),
        );
        command.env(
            "RUSTUP_HOME",
            std::env::var_os("RUSTUP_HOME").unwrap_or_else(|| host_home.join(".rustup").into()),
        );
    }

    fn run_release(&self, version: &str) -> Output {
        let mut command = Command::new(self.repo.join("scripts/release.sh"));
        command
            .current_dir(&self.repo)
            .args([version, "--remote", "fake", "--skip-checks"])
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.env_path)
            .env("OCM_GH_BIN", &self.ghx)
            .env("OCM_GITHUB_REPOSITORY", "openclaw/ocm");
        self.apply_toolchain_env(&mut command);
        command.output().unwrap()
    }

    fn run_update_version(&self, version: &str) -> Output {
        let mut command = Command::new(self.repo.join("scripts/update-version.sh"));
        command
            .current_dir(&self.repo)
            .arg(version)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.env_path);
        self.apply_toolchain_env(&mut command);
        command.output().unwrap()
    }

    fn git_output(&self, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(&self.repo)
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.env_path)
            .output()
            .unwrap()
    }

    fn git_stdout(&self, args: &[&str]) -> String {
        let output = self.git_output(args);
        assert!(output.status.success(), "{}", stderr(&output));
        stdout(&output)
    }

    fn remote_ref(&self, reference: &str) -> String {
        let output = Command::new(&self.git)
            .args(["ls-remote", &path_string(&self.remote), reference])
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.env_path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        stdout(&output)
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn merge_release_pr(&self, version: &str) -> String {
        let branch = format!("release/v{version}");
        assert!(
            self.git_output(&["switch", "main"]).status.success(),
            "failed to switch to main"
        );
        let merge = self.git_output(&["merge", "--squash", &branch]);
        assert!(merge.status.success(), "{}", stderr(&merge));
        let title = format!("chore(release): bump version to {version} (#42)");
        let commit = self.git_output(&["commit", "-m", &title]);
        assert!(commit.status.success(), "{}", stderr(&commit));
        let push = self.git_output(&["push", "fake", "main"]);
        assert!(push.status.success(), "{}", stderr(&push));
        self.git_stdout(&["rev-parse", "HEAD"]).trim().to_string()
    }

    fn record_ci(&self, head_sha: &str, status: &str, conclusion: &str) {
        fs::write(
            self.repo.join(".git/test-ci-run"),
            format!(
                "1|12345|{head_sha}|push|main|{status}|{conclusion}|https://github.com/openclaw/ocm/actions/runs/12345\n"
            ),
        )
        .unwrap();
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn write_release_fixture(path: &Path) {
    fs::create_dir_all(path.join("scripts")).unwrap();
    fs::write(
        path.join("Cargo.toml"),
        r#"[package]
name = "ocm"
version = "0.2.7"
edition = "2024"
"#,
    )
    .unwrap();
    fs::write(
        path.join("Cargo.lock"),
        r#"version = 4

[[package]]
name = "ocm"
version = "0.2.7"
"#,
    )
    .unwrap();
    fs::write(path.join("README.md"), "release fixture\n").unwrap();
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/main.rs"), "fn main() {}\n").unwrap();
}

fn copy_script(repo: &Path, relative: &str) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let destination = repo.join(relative);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    write_executable_script(&destination, &fs::read_to_string(source).unwrap());
}

fn init_release_repo(label: &str) -> ReleaseRepo {
    let root = TestDir::new(label);
    let repo = root.child("repo");
    let remote = root.child("remote.git");
    let home = root.child("home");
    let fake_bin = root.child("fake-bin");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();

    write_release_fixture(&repo);
    for script in [
        "scripts/release.sh",
        "scripts/update-version.sh",
        "scripts/validate-version.sh",
        "scripts/read-package-version.sh",
        "scripts/verify-release-ci.sh",
        "scripts/verify-release-tag.sh",
    ] {
        copy_script(&repo, script);
    }

    let git_lookup = Command::new("sh")
        .args(["-lc", "command -v git"])
        .output()
        .unwrap();
    assert!(git_lookup.status.success(), "{}", stderr(&git_lookup));
    let real_git = stdout(&git_lookup).trim().to_string();

    write_executable_script(
        &fake_bin.join("git"),
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
real_git="{}"
if [[ "${{1:-}}" == "-c" && "${{2:-}}" == "tag.gpgSign=true" && "${{3:-}}" == "tag" ]]; then
  shift 2
  "$real_git" "$@"
  touch ".git/test-signed-${{3}}"
  exit 0
fi
if [[ "${{1:-}}" == "cat-file" && "${{2:-}}" == "-p" && -n "${{3:-}}" && -f ".git/test-signed-${{3}}" ]]; then
  printf '%s\n' '-----BEGIN SSH SIGNATURE-----'
  exit 0
fi
if [[ "${{1:-}}" == "verify-tag" ]]; then
  [[ -n "${{2:-}}" && -f ".git/test-signed-${{2}}" ]]
  exit
fi
exec "$real_git" "$@"
"#,
            real_git
        ),
    );

    let ghx = fake_bin.join("ghx");
    write_executable_script(
        &ghx,
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>.git/test-ghx-commands
if [[ "${1:-}" == "api" ]]; then
  endpoint=""
  for arg in "$@"; do
    case "$arg" in
      repos/*) endpoint="$arg" ;;
    esac
  done
  case "$endpoint" in
    repos/openclaw/ocm/actions/workflows/ci.yml/runs\?*)
      [[ -f .git/test-ci-run ]] || {
        printf '0|||||||\n'
        exit 0
      }
      cat .git/test-ci-run
      exit 0
      ;;
    repos/openclaw/ocm/git/ref/tags/*)
      tag="${endpoint##*/}"
      printf 'tag\t%s\n' "$(git rev-parse "${tag}^{tag}")"
      exit 0
      ;;
    repos/openclaw/ocm/git/tags/*)
      tag="$(git tag --points-at HEAD | head -n1)"
      printf '%s\tcommit\t%s\ttrue\n' "$tag" "$(git rev-parse "${tag}^{commit}")"
      exit 0
      ;;
    repos/openclaw/ocm)
      printf 'main\n'
      exit 0
      ;;
    repos/openclaw/ocm/git/ref/heads/main)
      git rev-parse main
      exit 0
      ;;
    repos/openclaw/ocm/compare/*)
      printf 'identical\n'
      exit 0
      ;;
  esac
  if [[ "$endpoint" == *"/commits/"*"/pulls"* ]]; then
    commit="$(git rev-parse HEAD)"
    version="$(./scripts/read-package-version.sh Cargo.toml Cargo.lock)"
    printf '42\tclosed\t2026-08-20T12:00:00Z\tmain\trelease/v%s\t%s\tchore(release): bump version to %s\n' \
      "$version" "$commit" "$version"
    exit 0
  fi
fi
case "${1:-} ${2:-}" in
  "pr view")
    [[ -f .git/test-pr-url ]] || exit 1
    [[ ! -f .git/test-pr-invalid ]] || exit 0
    cat .git/test-pr-url
    ;;
  "pr create")
    [[ ! -f .git/test-ghx-create-fails ]] || exit 1
    printf '%s\n' 'https://github.com/openclaw/ocm/pull/42' | tee .git/test-pr-url
    ;;
  "release view")
    exit 1
    ;;
  "workflow run")
    ;;
  *)
    exit 1
    ;;
esac
"#,
    );

    let env_path = format!(
        "{}:{}",
        path_string(&fake_bin),
        std::env::var("PATH").unwrap()
    );

    for args in [
        vec!["init".to_string()],
        vec!["checkout".to_string(), "-B".to_string(), "main".to_string()],
        vec![
            "config".to_string(),
            "user.name".to_string(),
            "Test User".to_string(),
        ],
        vec![
            "config".to_string(),
            "user.email".to_string(),
            "test@example.com".to_string(),
        ],
    ] {
        let output = Command::new(&real_git)
            .current_dir(&repo)
            .args(args)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &env_path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }

    let add = Command::new(&real_git)
        .current_dir(&repo)
        .args(["add", "."])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &env_path)
        .output()
        .unwrap();
    assert!(add.status.success(), "{}", stderr(&add));
    let commit = Command::new(&real_git)
        .current_dir(&repo)
        .args(["commit", "-m", "chore: seed release fixture"])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &env_path)
        .output()
        .unwrap();
    assert!(commit.status.success(), "{}", stderr(&commit));
    let remote_init = Command::new(&real_git)
        .args(["init", "--bare", &path_string(&remote)])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &env_path)
        .output()
        .unwrap();
    assert!(remote_init.status.success(), "{}", stderr(&remote_init));
    let remote_add = Command::new(&real_git)
        .current_dir(&repo)
        .args(["remote", "add", "fake", &path_string(&remote)])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &env_path)
        .output()
        .unwrap();
    assert!(remote_add.status.success(), "{}", stderr(&remote_add));
    let push = Command::new(&real_git)
        .current_dir(&repo)
        .args(["push", "-u", "fake", "main"])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &env_path)
        .output()
        .unwrap();
    assert!(push.status.success(), "{}", stderr(&push));

    ReleaseRepo {
        _root: root,
        repo,
        remote,
        home,
        env_path,
        git: real_git,
        ghx,
    }
}

#[test]
fn release_script_prepares_a_pull_request_without_mutating_remote_main() {
    let repo = init_release_repo("release-pr-prepare");
    let original_main = repo.remote_ref("refs/heads/main");

    let output = repo.run_release("0.2.8");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("https://github.com/openclaw/ocm/pull/42"));
    assert_eq!(
        repo.git_stdout(&["branch", "--show-current"]).trim(),
        "release/v0.2.8"
    );
    assert_eq!(
        repo.git_stdout(&["log", "-1", "--pretty=%s"]).trim(),
        "chore(release): bump version to 0.2.8"
    );
    assert_eq!(repo.remote_ref("refs/heads/main"), original_main);
    assert_eq!(
        repo.remote_ref("refs/heads/release/v0.2.8"),
        repo.git_stdout(&["rev-parse", "HEAD"]).trim()
    );
    assert!(repo.remote_ref("refs/tags/v0.2.8").is_empty());
    assert!(
        fs::read_to_string(repo.repo.join(".git/test-ghx-commands"))
            .unwrap()
            .contains("pr create --repo openclaw/ocm")
    );
}

#[test]
fn release_script_resumes_the_release_branch_without_repeating_checks_or_commits() {
    let repo = init_release_repo("release-pr-resume");
    assert!(repo.run_release("0.2.8").status.success());
    let release_head = repo.git_stdout(&["rev-parse", "HEAD"]);

    let output = repo.run_release("0.2.8");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("skipping completed checks"));
    assert_eq!(repo.git_stdout(&["rev-parse", "HEAD"]), release_head);
    let commands = fs::read_to_string(repo.repo.join(".git/test-ghx-commands")).unwrap();
    assert!(commands.contains("pr view release/v0.2.8"));
}

#[test]
fn release_script_tags_only_after_the_release_pr_is_squash_merged() {
    let repo = init_release_repo("release-pr-tag");
    assert!(repo.run_release("0.2.8").status.success());
    let merged_head = repo.merge_release_pr("0.2.8");
    repo.record_ci(&merged_head, "completed", "success");

    let output = repo.run_release("0.2.8");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("protected-main workflow"));
    assert_eq!(repo.remote_ref("refs/heads/main"), merged_head);
    assert_eq!(repo.remote_ref("refs/tags/v0.2.8^{}"), merged_head);
}

#[test]
fn release_script_retries_a_previously_created_local_tag() {
    let repo = init_release_repo("release-local-tag-resume");
    assert!(repo.run_release("0.2.8").status.success());
    let merged_head = repo.merge_release_pr("0.2.8");
    repo.record_ci(&merged_head, "completed", "success");
    let tag = repo.git_output(&[
        "-c",
        "tag.gpgSign=true",
        "tag",
        "-a",
        "v0.2.8",
        "-m",
        "v0.2.8",
    ]);
    assert!(tag.status.success(), "{}", stderr(&tag));

    let output = repo.run_release("0.2.8");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("Using existing local tag v0.2.8"));
    assert_eq!(repo.remote_ref("refs/tags/v0.2.8^{}"), merged_head);
}

#[test]
fn release_script_accepts_an_already_pushed_verified_tag() {
    let repo = init_release_repo("release-tag-idempotent");
    assert!(repo.run_release("0.2.8").status.success());
    let merged_head = repo.merge_release_pr("0.2.8");
    repo.record_ci(&merged_head, "completed", "success");
    assert!(repo.run_release("0.2.8").status.success());

    let output = repo.run_release("0.2.8");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("already published from verified commit"));
}

#[test]
fn release_script_refuses_to_tag_without_exact_sha_ci() {
    let repo = init_release_repo("release-ci-missing");
    assert!(repo.run_release("0.2.8").status.success());
    let merged_head = repo.merge_release_pr("0.2.8");

    let output = repo.run_release("0.2.8");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains(&format!(
        "no main-branch CI push run exists for exact commit {merged_head}"
    )));
    assert!(repo.remote_ref("refs/tags/v0.2.8").is_empty());
}

#[test]
fn release_script_refuses_pending_failed_and_wrong_sha_ci() {
    let repo = init_release_repo("release-ci-invalid");
    assert!(repo.run_release("0.2.8").status.success());
    let merged_head = repo.merge_release_pr("0.2.8");

    repo.record_ci(&merged_head, "in_progress", "");
    let pending = repo.run_release("0.2.8");
    assert_eq!(pending.status.code(), Some(1));
    assert!(stderr(&pending).contains("is still in_progress"));

    repo.record_ci(&merged_head, "completed", "failure");
    let failed = repo.run_release("0.2.8");
    assert_eq!(failed.status.code(), Some(1));
    assert!(stderr(&failed).contains("concluded failure"));

    let wrong_sha = repo.git_stdout(&["rev-parse", "HEAD^"]);
    repo.record_ci(wrong_sha.trim(), "completed", "success");
    let wrong = repo.run_release("0.2.8");
    assert_eq!(wrong.status.code(), Some(1));
    assert!(stderr(&wrong).contains(&format!("not {merged_head}")));
    assert!(repo.remote_ref("refs/tags/v0.2.8").is_empty());
}

#[test]
fn release_script_fails_when_the_pull_request_cannot_be_created() {
    let repo = init_release_repo("release-pr-create-failure");
    fs::write(repo.repo.join(".git/test-ghx-create-fails"), "").unwrap();

    let output = repo.run_release("0.2.8");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("failed to create the release pull request"));
    assert!(!stdout(&output).contains("Release pull request ready"));
}

#[test]
fn release_script_replaces_an_unsuitable_existing_pull_request() {
    let repo = init_release_repo("release-pr-replace-invalid");
    assert!(repo.run_release("0.2.8").status.success());
    fs::write(repo.repo.join(".git/test-pr-invalid"), "").unwrap();

    let output = repo.run_release("0.2.8");
    assert!(output.status.success(), "{}", stderr(&output));
    let commands = fs::read_to_string(repo.repo.join(".git/test-ghx-commands")).unwrap();
    assert_eq!(
        commands
            .lines()
            .filter(|line| line.starts_with("pr create"))
            .count(),
        2
    );
}

#[test]
fn release_script_refuses_unrelated_dirty_changes() {
    let repo = init_release_repo("release-refuses-dirty");
    fs::write(repo.repo.join("README.md"), "dirty\n").unwrap();

    let output = repo.run_release("0.2.8");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("tracked changes are present"));
    assert!(repo.remote_ref("refs/heads/release/v0.2.8").is_empty());
}

#[test]
fn update_version_rejects_invalid_semver_without_mutating_files() {
    let repo = init_release_repo("update-version-invalid");
    let manifest = fs::read(repo.repo.join("Cargo.toml")).unwrap();
    let lockfile = fs::read(repo.repo.join("Cargo.lock")).unwrap();

    let output = repo.run_update_version("0.2.8..1");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("invalid semantic version"));
    assert_eq!(fs::read(repo.repo.join("Cargo.toml")).unwrap(), manifest);
    assert_eq!(fs::read(repo.repo.join("Cargo.lock")).unwrap(), lockfile);
}

#[test]
fn update_version_accepts_semver_build_metadata_without_running_cargo() {
    let repo = init_release_repo("update-version-metadata");
    let output = repo.run_update_version("0.2.8+build.1");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        fs::read_to_string(repo.repo.join("Cargo.toml"))
            .unwrap()
            .contains("version = \"0.2.8+build.1\"")
    );
}
