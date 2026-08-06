mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::support::{TestDir, path_string, write_executable_script};

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name)
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn make_valid_archive(path: &Path, root: &TestDir) {
    let contents = root.child("archive-contents");
    fs::create_dir_all(&contents).unwrap();
    fs::write(contents.join("ocm"), "binary").unwrap();
    fs::write(contents.join("LICENSE"), "license").unwrap();
    fs::write(contents.join("README.md"), "readme").unwrap();
    let output = Command::new("tar")
        .args(["-czf"])
        .arg(path)
        .arg("-C")
        .arg(&contents)
        .args(["ocm", "LICENSE", "README.md"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
}

fn populate_release_archives(asset_dir: &Path, root: &TestDir) {
    fs::create_dir_all(asset_dir).unwrap();
    fs::write(asset_dir.join("install.sh"), "#!/usr/bin/env bash\n").unwrap();
    let source = root.child("source.tar.gz");
    make_valid_archive(&source, root);
    for name in [
        "ocm-aarch64-apple-darwin.tar.gz",
        "ocm-x86_64-apple-darwin.tar.gz",
        "ocm-x86_64-unknown-linux-gnu.tar.gz",
    ] {
        fs::copy(&source, asset_dir.join(name)).unwrap();
    }
}

#[test]
fn package_release_preserves_existing_archive_when_tar_fails() {
    let root = TestDir::new("package-release-atomic");
    let output_dir = root.child("dist");
    let fake_bin = root.child("bin");
    fs::create_dir_all(&output_dir).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    let archive = output_dir.join("ocm-test-target.tar.gz");
    fs::write(&archive, "known-good").unwrap();
    let binary = root.child("ocm");
    fs::write(&binary, "binary").unwrap();
    write_executable_script(
        &fake_bin.join("tar"),
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf partial >\"$2\"\nexit 1\n",
    );
    let path = format!(
        "{}:{}",
        path_string(&fake_bin),
        std::env::var("PATH").unwrap()
    );

    let output = Command::new(script("package-release.sh"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["--target", "test-target", "--binary"])
        .arg(&binary)
        .arg("--output-dir")
        .arg(&output_dir)
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(fs::read(&archive).unwrap(), b"known-good");
    assert!(fs::read_dir(&output_dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".ocm-")
    }));
}

#[test]
fn prepare_release_assets_requires_the_complete_matrix_and_writes_checksums() {
    let root = TestDir::new("prepare-release-assets");
    let asset_dir = root.child("dist");
    populate_release_archives(&asset_dir, &root);

    let output = Command::new(script("prepare-release-assets.sh"))
        .arg(&asset_dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));

    let checksums = fs::read_to_string(asset_dir.join("SHA256SUMS")).unwrap();
    assert_eq!(checksums.lines().count(), 4);
    for name in [
        "ocm-aarch64-apple-darwin.tar.gz",
        "ocm-x86_64-apple-darwin.tar.gz",
        "ocm-x86_64-unknown-linux-gnu.tar.gz",
    ] {
        assert!(checksums.contains(name));
    }
    assert!(checksums.contains("install.sh"));

    fs::remove_file(asset_dir.join("ocm-aarch64-apple-darwin.tar.gz")).unwrap();
    let missing = Command::new(script("prepare-release-assets.sh"))
        .arg(&asset_dir)
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("required release archive is missing"));
}

#[test]
fn publish_release_keeps_the_release_draft_until_every_asset_is_uploaded() {
    let root = TestDir::new("publish-release-draft-first");
    let asset_dir = root.child("dist");
    let fake_bin = root.child("bin");
    let state_dir = root.child("state");
    fs::create_dir_all(&fake_bin).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    populate_release_archives(&asset_dir, &root);

    write_executable_script(
        &fake_bin.join("gh"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${TEST_GH_STATE}/commands"
case "${1:-} ${2:-}" in
  "release view")
    if [[ "$*" == *"--json isDraft"* ]]; then
      [[ -f "${TEST_GH_STATE}/draft" ]] || exit 1
      cat "${TEST_GH_STATE}/draft"
    else
      cat "${TEST_GH_STATE}/assets"
    fi
    ;;
  "release create")
    printf 'true\n' >"${TEST_GH_STATE}/draft"
    ;;
  "release upload")
    : >"${TEST_GH_STATE}/assets"
    for arg in "$@"; do
      case "$arg" in
        *.tar.gz|*/SHA256SUMS|*/install.sh) basename "$arg" >>"${TEST_GH_STATE}/assets" ;;
      esac
    done
    sort -o "${TEST_GH_STATE}/assets" "${TEST_GH_STATE}/assets"
    ;;
  "release edit")
    grep -q -- '--draft=false' <<<"$*"
    printf 'false\n' >"${TEST_GH_STATE}/draft"
    ;;
esac
"#,
    );
    let path = format!(
        "{}:{}",
        path_string(&fake_bin),
        std::env::var("PATH").unwrap()
    );

    let output = Command::new(script("publish-release.sh"))
        .args([
            "--repo",
            "example/ocm",
            "--tag",
            "v1.2.3+build-1",
            "--asset-dir",
        ])
        .arg(&asset_dir)
        .env("PATH", path)
        .env("TEST_GH_STATE", &state_dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));

    let commands = fs::read_to_string(state_dir.join("commands")).unwrap();
    let create = commands.find("release create").unwrap();
    let upload = commands.find("release upload").unwrap();
    let publish = commands.find("release edit").unwrap();
    assert!(create < upload && upload < publish);
    let publish_command = commands
        .lines()
        .find(|line| line.starts_with("release edit"))
        .unwrap();
    assert!(publish_command.contains("--draft=false"));
    assert!(publish_command.contains("--latest"));
    assert!(!publish_command.contains("--prerelease"));
    assert_eq!(
        fs::read_to_string(state_dir.join("draft")).unwrap(),
        "false\n"
    );
}

#[test]
fn verify_release_tag_requires_a_verified_annotated_tag_matching_the_package() {
    let root = TestDir::new("verify-release-tag");
    let fake_bin = root.child("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let expected_commit_output = Command::new("git")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(expected_commit_output.status.success());
    let expected_commit = String::from_utf8(expected_commit_output.stdout)
        .unwrap()
        .trim()
        .to_string();
    let committed_manifest_output = Command::new("git")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["show", &format!("{expected_commit}:Cargo.toml")])
        .output()
        .unwrap();
    assert!(committed_manifest_output.status.success());
    let committed_manifest = String::from_utf8(committed_manifest_output.stdout).unwrap();
    let committed_version = committed_manifest
        .lines()
        .find_map(|line| {
            line.strip_prefix("version = \"")
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap();
    let package_tag = format!("v{committed_version}");
    let tag_object = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    write_executable_script(
        &fake_bin.join("gh"),
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *"/git/ref/tags/"*) printf 'tag\t{tag_object}\n' ;;
  *"/git/tags/"*) printf '%s\tcommit\t{expected_commit}\t%s\n' "${{TEST_TAG:-{package_tag}}}" "${{TEST_TAG_VERIFIED:-true}}" ;;
  *"/git/ref/heads/main"*) printf '{expected_commit}\n' ;;
  *"/compare/"*) printf '%s\n' "${{TEST_COMPARE_STATUS:-identical}}" ;;
  "api repos/example/ocm --jq .default_branch") printf 'main\n' ;;
  *) exit 1 ;;
esac
"#
        ),
    );
    let path = format!(
        "{}:{}",
        path_string(&fake_bin),
        std::env::var("PATH").unwrap()
    );

    let verified = Command::new(script("verify-release-tag.sh"))
        .args(["--repo", "example/ocm", "--tag"])
        .arg(&package_tag)
        .args(["--commit", &expected_commit])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(verified.status.success(), "{}", stderr(&verified));

    let unsigned = Command::new(script("verify-release-tag.sh"))
        .args(["--repo", "example/ocm", "--tag"])
        .arg(&package_tag)
        .args(["--commit", &expected_commit])
        .env("PATH", &path)
        .env("TEST_TAG_VERIFIED", "false")
        .output()
        .unwrap();
    assert_eq!(unsigned.status.code(), Some(1));
    assert!(stderr(&unsigned).contains("did not verify the signature"));

    let mismatched = Command::new(script("verify-release-tag.sh"))
        .args([
            "--repo",
            "example/ocm",
            "--tag",
            "v999.999.999",
            "--commit",
            &expected_commit,
        ])
        .env("PATH", &path)
        .env("TEST_TAG", "v999.999.999")
        .output()
        .unwrap();
    assert_eq!(mismatched.status.code(), Some(1));
    assert!(stderr(&mismatched).contains("does not match package version"));

    let unreviewed = Command::new(script("verify-release-tag.sh"))
        .args(["--repo", "example/ocm", "--tag"])
        .arg(&package_tag)
        .args(["--commit", &expected_commit])
        .env("PATH", path)
        .env("TEST_COMPARE_STATUS", "diverged")
        .output()
        .unwrap();
    assert_eq!(unreviewed.status.code(), Some(1));
    assert!(stderr(&unreviewed).contains("is not on the protected main branch"));
}

#[test]
fn installer_rejects_an_archive_that_does_not_match_release_checksums() {
    let root = TestDir::new("installer-checksum");
    let downloads = root.child("downloads");
    let fake_bin = root.child("bin");
    let bin_dir = root.child("installed");
    fs::create_dir_all(&downloads).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    let archive = downloads.join("ocm-x86_64-apple-darwin.tar.gz");
    make_valid_archive(&archive, &root);
    let digest_output = if Command::new("sha256sum")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        Command::new("sha256sum").arg(&archive).output().unwrap()
    } else {
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(&archive)
            .output()
            .unwrap()
    };
    assert!(digest_output.status.success());
    let digest = String::from_utf8(digest_output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    fs::write(
        downloads.join("SHA256SUMS"),
        format!("{digest}  ocm-x86_64-apple-darwin.tar.gz\n"),
    )
    .unwrap();
    write_executable_script(
        &fake_bin.join("curl"),
        r#"#!/usr/bin/env bash
set -euo pipefail
url=""
output=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o) shift; output="$1" ;;
    http*) url="$1" ;;
  esac
  shift
done
cp "${TEST_DOWNLOADS}/${url##*/}" "$output"
"#,
    );
    write_executable_script(
        &fake_bin.join("uname"),
        "#!/usr/bin/env bash\n[[ \"${1:-}\" == \"-s\" ]] && echo Darwin || echo x86_64\n",
    );
    let path = format!(
        "{}:{}",
        path_string(&fake_bin),
        std::env::var("PATH").unwrap()
    );

    fs::write(&archive, "tampered").unwrap();
    let output = Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
        .args(["--version", "v1.2.3", "--bin-dir"])
        .arg(&bin_dir)
        .env("PATH", path)
        .env("TEST_DOWNLOADS", &downloads)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("checksum mismatch"));
    assert!(!bin_dir.join("ocm").exists());
}

#[test]
fn workflows_pin_actions_lock_dependencies_and_gate_the_msrv() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ci = fs::read_to_string(root.join(".github/workflows/ci.yml")).unwrap();
    let release = fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
    let cargo = fs::read_to_string(root.join("Cargo.toml")).unwrap();

    for workflow in [&ci, &release] {
        for line in workflow.lines().map(str::trim) {
            let Some(reference) = line.strip_prefix("- uses: ") else {
                continue;
            };
            let sha = reference
                .split_once('@')
                .unwrap()
                .1
                .split_whitespace()
                .next()
                .unwrap();
            assert_eq!(sha.len(), 40, "mutable action reference: {reference}");
            assert!(sha.chars().all(|character| character.is_ascii_hexdigit()));
        }
    }

    assert!(cargo.contains("rust-version = \"1.88\""));
    assert!(ci.contains("toolchain: 1.88.0"));
    assert!(ci.contains("cargo check --workspace --all-targets --locked"));
    assert!(ci.contains("cargo test --locked"));
    assert!(ci.contains("cargo install --locked"));
    assert!(release.contains("cargo build --locked --release"));
    assert!(release.contains("scripts/verify-release-tag.sh"));
    assert!(release.contains("scripts/verify-release-ci.sh"));
    assert!(release.contains("scripts/publish-release.sh"));
    assert!(release.contains("actions: read"));
    assert!(release.contains("cp ./install.sh ./dist/install.sh"));
    assert!(release.contains("tags:"));
    assert!(release.contains("- \"v*\""));
    assert!(release.contains("github.ref_name"));
    assert!(release.contains("group: release-${{ github.ref_name }}"));
    assert!(!release.contains("repository_dispatch:"));
    assert!(!release.contains("github.event.client_payload.tag"));
    assert!(!release.contains("workflow_dispatch:"));
    assert!(release.contains("os: macos-15-intel"));
    assert!(!release.contains("os: macos-13"));
}
