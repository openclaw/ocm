mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use flate2::{Compression, write::GzEncoder};
use tar::{Builder, EntryType, Header};

use crate::support::{TestDir, path_string, write_executable_script};

const SUPPORTED_TARGETS: [&str; 3] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];

const RELEASE_ARCHIVES: [&str; 3] = [
    "ocm-aarch64-apple-darwin.tar.gz",
    "ocm-x86_64-apple-darwin.tar.gz",
    "ocm-x86_64-unknown-linux-gnu.tar.gz",
];

const RELEASE_ASSETS: [&str; 5] = [
    "SHA256SUMS",
    "install.sh",
    "ocm-aarch64-apple-darwin.tar.gz",
    "ocm-x86_64-apple-darwin.tar.gz",
    "ocm-x86_64-unknown-linux-gnu.tar.gz",
];

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name)
}

struct ReleaseCommitFixture {
    _root: TestDir,
    repo: PathBuf,
    commit: String,
}

impl ReleaseCommitFixture {
    fn new(label: &str) -> Self {
        let root = TestDir::new(label);
        let repo = root.child("repo");
        fs::create_dir_all(repo.join("scripts")).unwrap();
        for name in [
            "prepare-release-assets.sh",
            "publish-release.sh",
            "read-package-version.sh",
            "validate-version.sh",
            "verify-release-ci.sh",
            "verify-release-tag.sh",
        ] {
            write_executable_script(
                &repo.join("scripts").join(name),
                &fs::read_to_string(script(name)).unwrap(),
            );
        }
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"ocm\"\nversion = \"0.2.31\"\nrepository = \"https://github.com/shakkernerd/ocm\"\n",
        )
        .unwrap();
        fs::write(
            repo.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"ocm\"\nversion = \"0.2.31\"\n",
        )
        .unwrap();

        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test User"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "."],
            vec!["commit", "-m", "chore: seed release fixture"],
        ] {
            let output = Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", stderr(&output));
        }

        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"ocm\"\nversion = \"0.2.32\"\nrepository = \"https://github.com/shakkernerd/ocm\"\n",
        )
        .unwrap();
        fs::write(
            repo.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"ocm\"\nversion = \"0.2.32\"\n",
        )
        .unwrap();
        for args in [
            vec!["add", "Cargo.toml", "Cargo.lock"],
            vec![
                "commit",
                "-m",
                "chore(release): bump version to 0.2.32 (#81)",
            ],
        ] {
            let output = Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", stderr(&output));
        }
        let output = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        let commit = String::from_utf8(output.stdout).unwrap().trim().to_string();

        Self {
            _root: root,
            repo,
            commit,
        }
    }

    fn script(&self, name: &str) -> PathBuf {
        self.repo.join("scripts").join(name)
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

enum TestArchiveEntry<'a> {
    Regular(&'a str),
    Symlink(&'a str, &'a str),
    HardLink(&'a str, &'a str),
    Fifo(&'a str),
}

fn make_archive(path: &Path, entries: &[TestArchiveEntry<'_>]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let file = fs::File::create(path).unwrap();
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);
    for entry in entries {
        let mut header = Header::new_gnu();
        match entry {
            TestArchiveEntry::Regular(name) => {
                let contents = format!("contents for {name}\n");
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, name, contents.as_bytes())
                    .unwrap();
            }
            TestArchiveEntry::Symlink(name, target) => {
                header.set_entry_type(EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                header.set_link_name(target).unwrap();
                header.set_cksum();
                builder
                    .append_data(&mut header, name, std::io::empty())
                    .unwrap();
            }
            TestArchiveEntry::HardLink(name, target) => {
                header.set_entry_type(EntryType::Link);
                header.set_size(0);
                header.set_mode(0o644);
                header.set_link_name(target).unwrap();
                header.set_cksum();
                builder
                    .append_data(&mut header, name, std::io::empty())
                    .unwrap();
            }
            TestArchiveEntry::Fifo(name) => {
                header.set_entry_type(EntryType::Fifo);
                header.set_size(0);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, name, std::io::empty())
                    .unwrap();
            }
        }
    }
    builder.finish().unwrap();
    builder.into_inner().unwrap().finish().unwrap();
}

fn make_valid_archive(path: &Path, _root: &TestDir) {
    make_archive(
        path,
        &[
            TestArchiveEntry::Regular("ocm"),
            TestArchiveEntry::Regular("LICENSE"),
            TestArchiveEntry::Regular("README.md"),
        ],
    );
}

fn archive_listing(path: &Path, verbose: bool) -> Output {
    Command::new("tar")
        .arg(if verbose { "-tvzf" } else { "-tzf" })
        .arg(path)
        .output()
        .unwrap()
}

fn assert_exact_regular_archive(path: &Path) {
    let names = archive_listing(path, false);
    assert!(names.status.success(), "{}", stderr(&names));
    let mut names = String::from_utf8(names.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["LICENSE", "README.md", "ocm"]);

    let verbose = archive_listing(path, true);
    assert!(verbose.status.success(), "{}", stderr(&verbose));
    let verbose = String::from_utf8(verbose.stdout).unwrap();
    assert_eq!(verbose.lines().count(), 3);
    assert!(verbose.lines().all(|line| line.starts_with('-')));
}

fn populate_release_archives(asset_dir: &Path, root: &TestDir) {
    fs::create_dir_all(asset_dir).unwrap();
    fs::write(asset_dir.join("install.sh"), "#!/usr/bin/env bash\n").unwrap();
    let source = root.child("source.tar.gz");
    make_valid_archive(&source, root);
    for name in RELEASE_ARCHIVES {
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
    let archive = output_dir.join("ocm-x86_64-apple-darwin.tar.gz");
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
        .args(["--target", "x86_64-apple-darwin", "--binary"])
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
fn package_release_accepts_only_the_supported_matrix_and_emits_exact_bundles() {
    let root = TestDir::new("package-release-matrix");
    let output_dir = root.child("dist");
    let binary = root.child("ocm");
    fs::write(&binary, "binary").unwrap();

    for target in SUPPORTED_TARGETS {
        let output = Command::new(script("package-release.sh"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--target", target, "--binary"])
            .arg(&binary)
            .arg("--output-dir")
            .arg(&output_dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert_exact_regular_archive(&output_dir.join(format!("ocm-{target}.tar.gz")));
    }

    fs::write(output_dir.join("install.sh"), "#!/usr/bin/env bash\n").unwrap();
    let prepared = Command::new(script("prepare-release-assets.sh"))
        .arg(&output_dir)
        .output()
        .unwrap();
    assert!(prepared.status.success(), "{}", stderr(&prepared));

    for target in ["aarch64-unknown-linux-gnu", "test-target"] {
        let rejected_dir = root.child(format!("rejected-{target}"));
        let output = Command::new(script("package-release.sh"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--target", target, "--binary"])
            .arg(&binary)
            .arg("--output-dir")
            .arg(&rejected_dir)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(stderr(&output).contains("unsupported release target"));
        assert!(!rejected_dir.join(format!("ocm-{target}.tar.gz")).exists());
    }
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
    let records = checksums
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let digest = fields.next().unwrap();
            let name = fields.next().unwrap();
            assert!(fields.next().is_none());
            assert_eq!(digest.len(), 64);
            assert!(
                digest
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
            );
            name.to_string()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        records,
        [
            "install.sh",
            "ocm-aarch64-apple-darwin.tar.gz",
            "ocm-x86_64-apple-darwin.tar.gz",
            "ocm-x86_64-unknown-linux-gnu.tar.gz",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    );

    fs::remove_file(asset_dir.join("ocm-aarch64-apple-darwin.tar.gz")).unwrap();
    let missing = Command::new(script("prepare-release-assets.sh"))
        .arg(&asset_dir)
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("required release archive is missing"));
}

#[test]
fn prepare_release_assets_rejects_invalid_members_without_replacing_checksums() {
    let cases = [
        (
            "missing-ocm",
            vec![
                TestArchiveEntry::Regular("LICENSE"),
                TestArchiveEntry::Regular("README.md"),
            ],
        ),
        (
            "missing-license",
            vec![
                TestArchiveEntry::Regular("ocm"),
                TestArchiveEntry::Regular("README.md"),
            ],
        ),
        (
            "missing-readme",
            vec![
                TestArchiveEntry::Regular("ocm"),
                TestArchiveEntry::Regular("LICENSE"),
            ],
        ),
        (
            "qualified",
            vec![
                TestArchiveEntry::Regular("nested/ocm"),
                TestArchiveEntry::Regular("LICENSE"),
                TestArchiveEntry::Regular("README.md"),
            ],
        ),
        (
            "duplicate",
            vec![
                TestArchiveEntry::Regular("ocm"),
                TestArchiveEntry::Regular("ocm"),
                TestArchiveEntry::Regular("LICENSE"),
                TestArchiveEntry::Regular("README.md"),
            ],
        ),
        (
            "symlink",
            vec![
                TestArchiveEntry::Symlink("ocm", "LICENSE"),
                TestArchiveEntry::Regular("LICENSE"),
                TestArchiveEntry::Regular("README.md"),
            ],
        ),
        (
            "hard-link",
            vec![
                TestArchiveEntry::HardLink("ocm", "LICENSE"),
                TestArchiveEntry::Regular("LICENSE"),
                TestArchiveEntry::Regular("README.md"),
            ],
        ),
        (
            "fifo",
            vec![
                TestArchiveEntry::Fifo("ocm"),
                TestArchiveEntry::Regular("LICENSE"),
                TestArchiveEntry::Regular("README.md"),
            ],
        ),
        (
            "extra",
            vec![
                TestArchiveEntry::Regular("ocm"),
                TestArchiveEntry::Regular("LICENSE"),
                TestArchiveEntry::Regular("README.md"),
                TestArchiveEntry::Regular("NOTICE"),
            ],
        ),
    ];

    for (label, entries) in cases {
        let root = TestDir::new(&format!("prepare-invalid-{label}"));
        let asset_dir = root.child("dist");
        populate_release_archives(&asset_dir, &root);
        make_archive(&asset_dir.join("ocm-aarch64-apple-darwin.tar.gz"), &entries);
        fs::write(asset_dir.join("SHA256SUMS"), "known-good\n").unwrap();

        let output = Command::new(script("prepare-release-assets.sh"))
            .arg(&asset_dir)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{label}");
        assert_eq!(
            fs::read_to_string(asset_dir.join("SHA256SUMS")).unwrap(),
            "known-good\n",
            "{label}"
        );
    }
}

#[test]
fn prepare_release_assets_preserves_checksums_for_invalid_installer_or_matrix() {
    for case in [
        "installer-missing",
        "installer-symlink",
        "extra-archive",
        "extra-archive-symlink",
        "extra-archive-directory",
    ] {
        let root = TestDir::new(&format!("prepare-invalid-{case}"));
        let asset_dir = root.child("dist");
        populate_release_archives(&asset_dir, &root);
        fs::write(asset_dir.join("SHA256SUMS"), "known-good\n").unwrap();
        if case == "installer-missing" {
            fs::remove_file(asset_dir.join("install.sh")).unwrap();
        } else if case == "installer-symlink" {
            fs::remove_file(asset_dir.join("install.sh")).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(
                asset_dir.join("ocm-aarch64-apple-darwin.tar.gz"),
                asset_dir.join("install.sh"),
            )
            .unwrap();
        } else if case == "extra-archive" {
            fs::copy(
                asset_dir.join("ocm-aarch64-apple-darwin.tar.gz"),
                asset_dir.join("ocm-aarch64-unknown-linux-gnu.tar.gz"),
            )
            .unwrap();
        } else if case == "extra-archive-symlink" {
            #[cfg(unix)]
            std::os::unix::fs::symlink(
                asset_dir.join("ocm-aarch64-apple-darwin.tar.gz"),
                asset_dir.join("ocm-aarch64-unknown-linux-gnu.tar.gz"),
            )
            .unwrap();
        } else {
            fs::create_dir(asset_dir.join("ocm-aarch64-unknown-linux-gnu.tar.gz")).unwrap();
        }

        let output = Command::new(script("prepare-release-assets.sh"))
            .arg(&asset_dir)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{case}");
        assert_eq!(
            fs::read_to_string(asset_dir.join("SHA256SUMS")).unwrap(),
            "known-good\n",
            "{case}"
        );
    }
}

struct PublishResult {
    output: Output,
    commands: String,
    draft: Option<String>,
    assets: Vec<String>,
}

fn run_publish_scenario(
    tag: &str,
    authority: &str,
    lookup: &str,
    upload: &str,
    query: &str,
    valid_assets: bool,
) -> PublishResult {
    let verification = ReleaseCommitFixture::new("publish-release-verification");
    let root = TestDir::new("publish-release-draft-first");
    let asset_dir = root.child("dist");
    let fake_bin = root.child("bin");
    let state_dir = root.child("state");
    fs::create_dir_all(&fake_bin).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    populate_release_archives(&asset_dir, &root);
    if !valid_assets {
        fs::remove_file(asset_dir.join("ocm-aarch64-apple-darwin.tar.gz")).unwrap();
    }
    match lookup {
        "draft" => fs::write(state_dir.join("draft"), "true\n").unwrap(),
        "public" => fs::write(state_dir.join("draft"), "false\n").unwrap(),
        _ => {}
    }

    write_executable_script(
        &fake_bin.join("gh"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${TEST_GH_STATE}/commands"
if [[ "${1:-}" == "api" ]]; then
  endpoint=""
  for arg in "$@"; do
    case "$arg" in
      repos/*) endpoint="$arg" ;;
    esac
  done
  case "$endpoint" in
    repos/openclaw/ocm/actions/workflows/ci.yml/runs\?*)
      conclusion="success"
      [[ "$TEST_AUTHORITY" != "ci-fail" ]] || conclusion="failure"
      printf '1|12345|%s|push|main|completed|%s|https://github.com/openclaw/ocm/actions/runs/12345\n' "$TEST_EXPECTED_COMMIT" "$conclusion"
      exit 0
      ;;
    repos/openclaw/ocm/git/ref/tags/*)
      printf 'tag\tbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n'
      exit 0
      ;;
    repos/openclaw/ocm/git/tags/*)
      count_file="${TEST_GH_STATE}/tag-verifier-count"
      count=0
      [[ ! -f "$count_file" ]] || count="$(cat "$count_file")"
      count=$((count + 1))
      printf '%s\n' "$count" >"$count_file"
      target="$TEST_EXPECTED_COMMIT"
      verified="true"
      [[ "$TEST_AUTHORITY" != "initial-tag-fail" ]] || verified="false"
      if [[ "$TEST_AUTHORITY" == "retarget" && "$count" -gt 1 ]] ||
        [[ "$TEST_AUTHORITY" == "post-upload-retarget" && "$count" -gt 4 ]]; then
        target="0000000000000000000000000000000000000000"
      fi
      printf 'v0.2.32\tcommit\t%s\t%s\n' "$target" "$verified"
      exit 0
      ;;
    repos/openclaw/ocm/git/ref/heads/main)
      printf '%s\n' "$TEST_EXPECTED_COMMIT"
      exit 0
      ;;
    repos/openclaw/ocm/compare/*)
      printf 'identical\n'
      exit 0
      ;;
    repos/openclaw/ocm/commits/*/pulls)
      printf '81\tclosed\t2026-08-16T01:41:59Z\tmain\trelease/v0.2.32\t%s\tchore(release): bump version to 0.2.32\n' "$TEST_EXPECTED_COMMIT"
      exit 0
      ;;
    repos/openclaw/ocm)
      printf 'main\n'
      exit 0
      ;;
  esac
fi
case "${1:-} ${2:-}" in
  "release view")
    if [[ "$*" == *"--json isDraft"* ]]; then
      case "$TEST_LOOKUP" in
        missing) printf 'release not found\n' >&2; exit 1 ;;
        error) printf 'authentication failed\n' >&2; exit 1 ;;
        draft) printf 'true\n' ;;
        public) printf 'false\n' ;;
        *) exit 2 ;;
      esac
    else
      [[ "$TEST_QUERY" != "fail" ]] || {
        printf 'asset query failed\n' >&2
        exit 1
      }
      cat "${TEST_GH_STATE}/assets"
    fi
    ;;
  "release create")
    printf 'true\n' >"${TEST_GH_STATE}/draft"
    ;;
  "release upload")
    [[ "$TEST_UPLOAD" != "fail" ]] || {
      printf 'upload failed\n' >&2
      exit 1
    }
    : >"${TEST_GH_STATE}/assets"
    for arg in "$@"; do
      case "$arg" in
        *.tar.gz|*/SHA256SUMS|*/install.sh) basename "$arg" >>"${TEST_GH_STATE}/assets" ;;
      esac
    done
    sort -o "${TEST_GH_STATE}/assets" "${TEST_GH_STATE}/assets"
    if [[ "$TEST_UPLOAD" == "mismatch" ]]; then
      grep -v '^install\.sh$' "${TEST_GH_STATE}/assets" >"${TEST_GH_STATE}/assets.next"
      mv "${TEST_GH_STATE}/assets.next" "${TEST_GH_STATE}/assets"
    fi
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

    let output = Command::new(verification.script("publish-release.sh"))
        .current_dir(&verification.repo)
        .args([
            "--repo",
            "openclaw/ocm",
            "--tag",
            tag,
            "--commit",
            &verification.commit,
            "--asset-dir",
        ])
        .arg(&asset_dir)
        .env("PATH", path)
        .env("TEST_GH_STATE", &state_dir)
        .env("TEST_EXPECTED_COMMIT", &verification.commit)
        .env("TEST_AUTHORITY", authority)
        .env("TEST_LOOKUP", lookup)
        .env("TEST_UPLOAD", upload)
        .env("TEST_QUERY", query)
        .output()
        .unwrap();

    let commands = fs::read_to_string(state_dir.join("commands")).unwrap_or_default();
    let draft = fs::read_to_string(state_dir.join("draft")).ok();
    let assets = fs::read_to_string(state_dir.join("assets"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    PublishResult {
        output,
        commands,
        draft,
        assets,
    }
}

#[test]
fn publish_release_settles_stable_draft_after_authority_and_asset_verification() {
    let result = run_publish_scenario("v0.2.32", "success", "missing", "success", "success", true);
    assert!(result.output.status.success(), "{}", stderr(&result.output));
    assert_eq!(
        result.assets.into_iter().collect::<BTreeSet<_>>(),
        RELEASE_ASSETS
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(result.draft.as_deref(), Some("false\n"));

    let commands = result.commands;
    let tag_checks = commands
        .match_indices("/git/tags/")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(tag_checks.len(), 6);
    let ci = commands.find("actions/workflows/ci.yml/runs").unwrap();
    let create = commands.find("release create").unwrap();
    let upload = commands.find("release upload").unwrap();
    let query = commands.find("--json assets").unwrap();
    let publish = commands.find("release edit").unwrap();
    assert!(tag_checks[0] < tag_checks[1] && tag_checks[1] < ci);
    assert!(ci < tag_checks[2] && tag_checks[2] < tag_checks[3]);
    assert!(tag_checks[3] < create && create < upload && upload < query);
    assert!(query < tag_checks[4] && tag_checks[4] < tag_checks[5]);
    assert!(tag_checks[5] < publish);
    let publish_command = commands
        .lines()
        .find(|line| line.starts_with("release edit"))
        .unwrap();
    assert!(publish_command.contains("--draft=false"));
    assert!(publish_command.contains("--latest"));
    assert!(!publish_command.contains("--prerelease"));

    let publisher = fs::read_to_string(script("publish-release.sh")).unwrap();
    assert!(publisher.contains("release_flags+=(--prerelease --latest=false)"));
}

#[test]
fn publish_release_stops_before_release_api_when_authority_fails_or_tag_moves() {
    for authority in ["initial-tag-fail", "ci-fail", "retarget"] {
        let result =
            run_publish_scenario("v0.2.32", authority, "missing", "success", "success", true);
        assert_eq!(result.output.status.code(), Some(1), "{authority}");
        assert!(!result.commands.contains("release view"), "{authority}");
        assert!(!result.commands.contains("release create"), "{authority}");
        assert!(!result.commands.contains("release upload"), "{authority}");
        assert!(!result.commands.contains("release edit"), "{authority}");
    }
}

#[test]
fn publish_release_keeps_the_release_draft_when_tag_moves_after_upload() {
    let result = run_publish_scenario(
        "v0.2.32",
        "post-upload-retarget",
        "missing",
        "success",
        "success",
        true,
    );
    assert_eq!(result.output.status.code(), Some(1));
    assert!(result.commands.contains("release create"));
    assert!(result.commands.contains("release upload"));
    assert!(result.commands.contains("--json assets"));
    assert!(!result.commands.contains("release edit"));
    assert_eq!(result.draft.as_deref(), Some("true\n"));
}

#[test]
fn publish_release_distinguishes_missing_lookup_errors_and_public_releases() {
    let lookup_error =
        run_publish_scenario("v0.2.32", "success", "error", "success", "success", true);
    assert_eq!(lookup_error.output.status.code(), Some(1));
    assert!(stderr(&lookup_error.output).contains("failed to inspect existing release"));
    assert!(!lookup_error.commands.contains("release create"));
    assert!(!lookup_error.commands.contains("release upload"));
    assert!(!lookup_error.commands.contains("release edit"));

    let public = run_publish_scenario("v0.2.32", "success", "public", "success", "success", true);
    assert_eq!(public.output.status.code(), Some(1));
    assert!(stderr(&public.output).contains("already public"));
    assert!(!public.commands.contains("release create"));
    assert!(!public.commands.contains("release upload"));
    assert!(!public.commands.contains("release edit"));
    assert_eq!(public.draft.as_deref(), Some("false\n"));
}

#[test]
fn publish_release_keeps_new_and_existing_releases_draft_on_failures() {
    for (label, lookup, upload, query) in [
        ("new-upload", "missing", "fail", "success"),
        ("existing-upload", "draft", "fail", "success"),
        ("existing-query", "draft", "success", "fail"),
        ("existing-mismatch", "draft", "mismatch", "success"),
    ] {
        let result = run_publish_scenario("v0.2.32", "success", lookup, upload, query, true);
        assert_eq!(result.output.status.code(), Some(1), "{label}");
        assert!(!result.commands.contains("release edit"), "{label}");
        assert_eq!(result.draft.as_deref(), Some("true\n"), "{label}");
        if lookup == "draft" {
            assert!(!result.commands.contains("release create"), "{label}");
        } else {
            assert!(result.commands.contains("release create"), "{label}");
        }
    }
}

#[test]
fn publish_release_stops_before_release_api_when_asset_preparation_fails() {
    let result = run_publish_scenario("v0.2.32", "success", "missing", "success", "success", false);
    assert_eq!(result.output.status.code(), Some(1));
    assert!(result.commands.contains("/git/ref/tags/v0.2.32"));
    assert!(result.commands.contains("actions/workflows/ci.yml/runs"));
    assert!(!result.commands.contains("release view"));
    assert!(!result.commands.contains("release create"));
    assert!(!result.commands.contains("release upload"));
    assert!(!result.commands.contains("release edit"));
    assert!(result.draft.is_none());
    assert!(result.assets.is_empty());
}

#[test]
fn publish_release_upload_set_matches_checksum_payloads() {
    let result = run_publish_scenario("v0.2.32", "success", "missing", "success", "success", true);
    assert!(result.output.status.success(), "{}", stderr(&result.output));
    assert_eq!(
        result.assets.into_iter().collect::<BTreeSet<_>>(),
        RELEASE_ASSETS
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
    );

    let root = TestDir::new("publish-checksum-payloads");
    let asset_dir = root.child("dist");
    populate_release_archives(&asset_dir, &root);
    let prepared = Command::new(script("prepare-release-assets.sh"))
        .arg(&asset_dir)
        .output()
        .unwrap();
    assert!(prepared.status.success(), "{}", stderr(&prepared));
    let checksum_names = fs::read_to_string(asset_dir.join("SHA256SUMS"))
        .unwrap()
        .lines()
        .map(|line| line.split_whitespace().nth(1).unwrap().to_string())
        .collect::<BTreeSet<_>>();
    let upload_payloads = RELEASE_ASSETS
        .into_iter()
        .filter(|name| *name != "SHA256SUMS")
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert_eq!(checksum_names, upload_payloads);
}

#[test]
fn package_version_reader_requires_one_matching_local_ocm_record() {
    let root = TestDir::new("package-version-reader");
    let manifest = root.child("Cargo.toml");
    let lockfile = root.child("Cargo.lock");
    fs::write(
        &manifest,
        "[package]\nname = \"ocm\"\nversion = \"1.2.3\"\n",
    )
    .unwrap();
    fs::write(
        &lockfile,
        "version = 4\n\n[[package]]\nname = \"ocm\"\nversion = \"1.2.3\"\n",
    )
    .unwrap();

    let valid = Command::new(script("read-package-version.sh"))
        .args([&manifest, &lockfile])
        .output()
        .unwrap();
    assert!(valid.status.success(), "{}", stderr(&valid));
    assert_eq!(String::from_utf8(valid.stdout).unwrap(), "1.2.3\n");

    for (label, contents) in [
        (
            "mismatch",
            "version = 4\n\n[[package]]\nname = \"ocm\"\nversion = \"1.2.4\"\n",
        ),
        (
            "duplicate",
            "version = 4\n\n[[package]]\nname = \"ocm\"\nversion = \"1.2.3\"\n\n[[package]]\nname = \"ocm\"\nversion = \"1.2.3\"\n",
        ),
        (
            "sourced",
            "version = 4\n\n[[package]]\nname = \"ocm\"\nversion = \"1.2.3\"\nsource = \"registry+https://example.test/index\"\n",
        ),
        (
            "checksum",
            "version = 4\n\n[[package]]\nname = \"ocm\"\nversion = \"1.2.3\"\nchecksum = \"abc\"\n",
        ),
        ("missing", "version = 4\n"),
    ] {
        fs::write(&lockfile, contents).unwrap();
        let rejected = Command::new(script("read-package-version.sh"))
            .args([&manifest, &lockfile])
            .output()
            .unwrap();
        assert_eq!(rejected.status.code(), Some(1), "{label}");
    }
}

#[test]
fn verify_release_tag_requires_a_verified_annotated_tag_matching_the_package() {
    let verification = ReleaseCommitFixture::new("verify-release-tag-repo");
    let root = TestDir::new("verify-release-tag");
    let fake_bin = root.child("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let expected_commit = verification.commit.clone();
    let package_tag = "v0.2.32";
    let tag_object = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    write_executable_script(
        &fake_bin.join("gh"),
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
endpoint=""
for arg in "$@"; do
  case "$arg" in
    repos/*) endpoint="$arg" ;;
  esac
done
case "$endpoint" in
  repos/openclaw/ocm/git/ref/tags/*) printf 'tag\t{tag_object}\n' ;;
  repos/openclaw/ocm/git/tags/*) printf '%s\tcommit\t{expected_commit}\t%s\n' "${{TEST_TAG:-{package_tag}}}" "${{TEST_TAG_VERIFIED:-true}}" ;;
  repos/openclaw/ocm/git/ref/heads/main) printf '{expected_commit}\n' ;;
  repos/openclaw/ocm/compare/*) printf '%s\n' "${{TEST_COMPARE_STATUS:-identical}}" ;;
  repos/openclaw/ocm/commits/*/pulls) printf '81\tclosed\t2026-08-16T01:41:59Z\tmain\trelease/v0.2.32\t{expected_commit}\tchore(release): bump version to 0.2.32\n' ;;
  repos/openclaw/ocm) printf 'main\n' ;;
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

    let verified = Command::new(verification.script("verify-release-tag.sh"))
        .current_dir(&verification.repo)
        .args(["--repo", "openclaw/ocm", "--tag"])
        .arg(&package_tag)
        .args(["--commit", &expected_commit])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(verified.status.success(), "{}", stderr(&verified));

    let unsigned = Command::new(verification.script("verify-release-tag.sh"))
        .current_dir(&verification.repo)
        .args(["--repo", "openclaw/ocm", "--tag"])
        .arg(&package_tag)
        .args(["--commit", &expected_commit])
        .env("PATH", &path)
        .env("TEST_TAG_VERIFIED", "false")
        .output()
        .unwrap();
    assert_eq!(unsigned.status.code(), Some(1));
    assert!(stderr(&unsigned).contains("did not verify the signature"));

    let mismatched = Command::new(verification.script("verify-release-tag.sh"))
        .current_dir(&verification.repo)
        .args([
            "--repo",
            "openclaw/ocm",
            "--tag",
            "v0.2.31",
            "--commit",
            &expected_commit,
        ])
        .env("PATH", &path)
        .env("TEST_TAG", "v0.2.31")
        .output()
        .unwrap();
    assert_eq!(mismatched.status.code(), Some(1));
    assert!(stderr(&mismatched).contains("does not match package version"));

    let unreviewed = Command::new(verification.script("verify-release-tag.sh"))
        .current_dir(&verification.repo)
        .args(["--repo", "openclaw/ocm", "--tag"])
        .arg(&package_tag)
        .args(["--commit", &expected_commit])
        .env("PATH", path)
        .env("TEST_COMPARE_STATUS", "diverged")
        .output()
        .unwrap();
    assert_eq!(unreviewed.status.code(), Some(1));
    assert!(stderr(&unreviewed).contains("is not on protected main"));
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
    assert!(release.contains("pull-requests: read"));
    assert!(release.contains("workflow_dispatch:"));
    assert!(release.contains("group: release-${{ inputs.tag }}"));
    assert!(release.contains("github.repository == 'openclaw/ocm'"));
    assert!(release.contains("github.ref == 'refs/heads/main'"));
    assert!(release.contains("RELEASE_TAG: ${{ inputs.tag }}"));
    assert!(release.contains("source: ${{ steps.verify.outputs.source }}"));
    assert!(release.contains("ref: ${{ github.sha }}"));
    assert!(release.contains("ref: ${{ needs.verify.outputs.source }}"));
    assert!(release.contains("path: trusted"));
    assert!(release.contains("path: release-source"));
    assert!(release.contains("cp ./release-source/install.sh ./dist/install.sh"));
    assert!(release.contains("./trusted/scripts/publish-release.sh"));
    assert!(release.contains("--commit \"$RELEASE_COMMIT\""));
    assert!(!release.contains("inputs.commit"));
    assert!(!release.contains("needs.verify.outputs.commit"));
    assert!(!release.contains("Swatinem/rust-cache"));
    assert!(!release.contains("push:\n    tags:"));
    assert!(!release.contains("repository_dispatch:"));
    assert!(!release.contains("github.event.client_payload.tag"));
    assert!(release.contains("os: macos-15-intel"));
    assert!(!release.contains("os: macos-13"));

    let workflow_targets = release
        .lines()
        .filter_map(|line| line.trim().strip_prefix("target: "))
        .collect::<BTreeSet<_>>();
    assert_eq!(workflow_targets, SUPPORTED_TARGETS.into_iter().collect());
    assert!(release.contains("name: release-${{ matrix.target }}"));
    assert!(release.contains("path: ./dist/ocm-${{ matrix.target }}.tar.gz"));
    assert!(release.contains("pattern: release-*"));
    assert!(release.contains("merge-multiple: true"));
    assert!(release.contains("--target \"${{ matrix.target }}\""));
    assert!(release.contains("--binary \"./target/${{ matrix.target }}/release/ocm\""));
}
