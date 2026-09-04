use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use semver::Version;
use serde::Serialize;

use super::self_update_transaction::{Phase, Transaction};
use crate::infra::archive::extract_tar_gz;
use crate::infra::download::{download_to_file, http_agent, verify_file_sha256};

use super::{Cli, render};

const RELEASE_REPO: &str = "openclaw/ocm";
const INTERNAL_SELF_UPDATE_RELEASE_URL_ENV: &str = "OCM_INTERNAL_SELF_UPDATE_RELEASE_URL";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SelfUpdateMode {
    Check,
    Update,
}

impl SelfUpdateMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Update => "update",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SelfUpdateStatus {
    UpToDate,
    UpdateAvailable,
    Updated,
}

impl SelfUpdateStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::UpdateAvailable => "update_available",
            Self::Updated => "updated",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SelfUpdateSummary {
    pub mode: SelfUpdateMode,
    pub status: SelfUpdateStatus,
    pub current_version: String,
    pub target_version: String,
    pub binary_path: String,
    pub asset_name: String,
    pub daemon_refresh_required: bool,
    pub daemon_refresh_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_path: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct GitHubReleaseAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

struct SelfUpdateTempDir {
    path: PathBuf,
}

impl SelfUpdateTempDir {
    fn create() -> Result<Self, String> {
        let unique = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
        for attempt in 0..100 {
            let path = std::env::temp_dir().join(format!(
                "ocm-self-update-{}-{unique}-{attempt}",
                std::process::id()
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;

                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "failed to create self-update temporary directory {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        Err("failed to allocate a unique self-update temporary directory".to_string())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SelfUpdateTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub(super) struct StagedBinary {
    path: PathBuf,
}

impl StagedBinary {
    pub(super) fn copy_from(source: &Path, parent: &Path) -> Result<Self, String> {
        let metadata = fs::symlink_metadata(source)
            .map_err(|error| format!("failed to inspect release archive binary: {error}"))?;
        if !metadata.file_type().is_file() {
            return Err("release archive ocm entry is not a regular file".to_string());
        }

        let mut source_file = File::open(source)
            .map_err(|error| format!("failed to open release binary: {error}"))?;
        let (staged, mut staged_file) = Self::create(parent)?;
        io::copy(&mut source_file, &mut staged_file).map_err(|error| {
            format!(
                "failed to stage the updated ocm binary in {}: {error}",
                parent.display()
            )
        })?;
        staged_file
            .sync_all()
            .map_err(|error| format!("failed to sync staged ocm binary: {error}"))?;
        drop(staged_file);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&staged.path)
                .map_err(|error| error.to_string())?
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&staged.path, permissions).map_err(|error| error.to_string())?;
        }

        Ok(staged)
    }

    fn create(parent: &Path) -> Result<(Self, File), String> {
        let unique = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
        for attempt in 0..100 {
            let path = parent.join(format!(
                ".ocm-update-{}-{unique}-{attempt}",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;

                options.mode(0o700);
            }
            match options.open(&path) {
                Ok(file) => return Ok((Self { path }, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "failed to create staged ocm binary in {}: {error}",
                        parent.display()
                    ));
                }
            }
        }
        Err(format!(
            "failed to allocate a unique staged ocm binary in {}",
            parent.display()
        ))
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagedBinary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl Cli {
    pub(super) fn handle_self_update(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "self update")?;
        let (args, check) = Self::consume_flag(args, "--check");
        let (args, status) = Self::consume_flag(args, "--status");
        let (args, recover) = Self::consume_flag(args, "--recover");
        let (args, version) = Self::consume_option(args, "--version")?;
        let version = Self::require_option_value(version, "--version")?;
        Self::assert_no_extra_args(&args)?;

        if status || recover {
            if check || version.is_some() || (status && recover) {
                return Err("--status and --recover must be used alone".into());
            }
            let receipt = self.self_update_receipt(recover)?;
            self.print_json(&receipt)?;
            return Ok(
                if matches!(
                    receipt.phase,
                    Phase::RolledBack | Phase::RollbackFailed | Phase::Failed
                ) {
                    1
                } else {
                    0
                },
            );
        }

        let target_display = version.clone().unwrap_or_else(|| "latest".to_string());
        let summary = if check {
            self.self_update_check(version.as_deref())?
        } else {
            self.with_progress(format!("Updating ocm to {target_display}"), || {
                self.self_update_install(version.as_deref())
            })?
        };

        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::self_update::self_update(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn dispatch_self_command(
        &self,
        action: &str,
        args: Vec<String>,
    ) -> Result<i32, String> {
        match action {
            "" | "help" | "--help" | "-h" => self.dispatch_help_command(vec!["self".to_string()]),
            "update" => self.handle_self_update(args),
            other => Err(format!("unknown self command: {other}")),
        }
    }

    fn self_update_check(&self, version: Option<&str>) -> Result<SelfUpdateSummary, String> {
        let current_version = env!("CARGO_PKG_VERSION").to_string();
        let binary_path = self.current_binary_path()?;
        let asset_name = self.current_release_asset_name()?;
        let release = self.fetch_self_release(version)?;
        let target_version = display_version_from_tag(&release.tag_name)?;

        Ok(SelfUpdateSummary {
            mode: SelfUpdateMode::Check,
            status: if should_treat_target_as_current(
                &current_version,
                &target_version,
                version.is_some(),
            )? {
                SelfUpdateStatus::UpToDate
            } else {
                SelfUpdateStatus::UpdateAvailable
            },
            current_version,
            target_version,
            binary_path: binary_path.to_string_lossy().into_owned(),
            asset_name,
            daemon_refresh_required: false,
            daemon_refresh_note: None,
            receipt_path: None,
        })
    }

    fn self_update_install(&self, version: Option<&str>) -> Result<SelfUpdateSummary, String> {
        let current_version = env!("CARGO_PKG_VERSION").to_string();
        let binary_path = self.current_binary_path()?;
        let asset_name = self.current_release_asset_name()?;
        let transaction = Transaction::open(&binary_path, false)?;
        transaction.admit()?;
        let release = self.fetch_self_release(version)?;
        let target_version = display_version_from_tag(&release.tag_name)?;

        if should_treat_target_as_current(&current_version, &target_version, version.is_some())? {
            return Ok(SelfUpdateSummary {
                mode: SelfUpdateMode::Update,
                status: SelfUpdateStatus::UpToDate,
                current_version,
                target_version,
                binary_path: binary_path.to_string_lossy().into_owned(),
                asset_name,
                daemon_refresh_required: false,
                daemon_refresh_note: None,
                receipt_path: None,
            });
        }

        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| {
                format!(
                    "release {} does not publish an asset for this platform: {asset_name}",
                    release.tag_name
                )
            })?;
        let expected_sha256 = github_asset_sha256(asset)?;

        let parent = binary_path.parent().ok_or_else(|| {
            format!(
                "cannot update ocm because its binary path has no parent: {}",
                binary_path.display()
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;

        let temp_root = SelfUpdateTempDir::create()?;
        let archive_path = temp_root.path().join(&asset.name);
        download_to_file(&asset.browser_download_url, &archive_path)?;
        verify_file_sha256(&archive_path, expected_sha256)
            .map_err(|error| format!("failed to verify release asset {}: {error}", asset.name))?;
        let extract_dir = temp_root.path().join("extract");
        extract_tar_gz(&archive_path, &extract_dir)?;

        let extracted_binary = extract_dir.join("ocm");
        let staged_binary = StagedBinary::copy_from(&extracted_binary, parent)?;
        validate_staged_binary(staged_binary.path(), &target_version)?;
        // Downloads and extraction no longer belong to the interruptible caller
        // once the durable helper takes over.
        drop(temp_root);
        let receipt = transaction.launch(
            self,
            &binary_path,
            staged_binary,
            &current_version,
            &target_version,
        )?;
        if receipt.phase != Phase::Updated {
            return Err(format!(
                "self-update {:?}: {}; inspect `ocm self update --status`",
                receipt.phase,
                receipt
                    .error
                    .as_deref()
                    .unwrap_or("update did not complete")
            ));
        }

        Ok(SelfUpdateSummary {
            mode: SelfUpdateMode::Update,
            status: SelfUpdateStatus::Updated,
            current_version,
            target_version,
            binary_path: binary_path.to_string_lossy().into_owned(),
            asset_name,
            daemon_refresh_required: false,
            daemon_refresh_note: None,
            receipt_path: Some(
                super::self_update_transaction::receipt_path(&binary_path)?
                    .to_string_lossy()
                    .into_owned(),
            ),
        })
    }

    fn current_binary_path(&self) -> Result<PathBuf, String> {
        std::env::current_exe()
            .map_err(|error| format!("failed to resolve the current ocm binary: {error}"))
    }

    fn current_release_asset_name(&self) -> Result<String, String> {
        release_asset_name(std::env::consts::OS, std::env::consts::ARCH)
    }

    fn fetch_self_release(&self, version: Option<&str>) -> Result<GitHubRelease, String> {
        let url = self_release_url(&self.env, version);

        let response = http_agent()
            .get(&url)
            .header("User-Agent", &format!("ocm/{}", env!("CARGO_PKG_VERSION")))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .map_err(|error| format!("failed to query ocm releases: {error}"))?;
        serde_json::from_reader(response.into_body().into_reader())
            .map_err(|error| format!("failed to parse ocm release metadata: {error}"))
    }
}

fn self_release_url(env: &BTreeMap<String, String>, version: Option<&str>) -> String {
    if let Some(override_url) = env
        .get(INTERNAL_SELF_UPDATE_RELEASE_URL_ENV)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        override_url.to_string()
    } else if let Some(version) = version {
        format!(
            "https://api.github.com/repos/{RELEASE_REPO}/releases/tags/{}",
            normalize_release_tag(version)
        )
    } else {
        format!("https://api.github.com/repos/{RELEASE_REPO}/releases/latest")
    }
}

fn release_asset_name(os: &str, arch: &str) -> Result<String, String> {
    let os_target = match os {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        other => {
            return Err(format!(
                "ocm self update is not supported on this operating system yet: {other}"
            ));
        }
    };
    let arch_target = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => {
            return Err(format!(
                "ocm self update is not supported on this architecture yet: {other}"
            ));
        }
    };
    let target = format!("{arch_target}-{os_target}");
    if target == "aarch64-unknown-linux-gnu" {
        return Err(format!(
            "ocm self update is not supported on this platform yet: {target}"
        ));
    }
    Ok(format!("ocm-{target}.tar.gz"))
}

fn github_asset_sha256(asset: &GitHubReleaseAsset) -> Result<&str, String> {
    let digest = asset
        .digest
        .as_deref()
        .ok_or_else(|| format!("release asset {} does not include a digest", asset.name))?;
    let Some((algorithm, value)) = digest.split_once(':') else {
        return Err(format!(
            "release asset {} has an invalid digest: {digest}",
            asset.name
        ));
    };
    if algorithm != "sha256" {
        return Err(format!(
            "release asset {} uses unsupported digest algorithm: {algorithm}",
            asset.name
        ));
    }
    Ok(value)
}

pub(super) fn validate_staged_binary(path: &Path, target_version: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect staged ocm binary: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("staged ocm binary is not a regular file".to_string());
    }

    let mut capture = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut child = Command::new(path)
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(capture.try_clone().map_err(|e| e.to_string())?)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to execute staged ocm binary: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    "staged ocm binary version check did not finish within 5 seconds".into(),
                );
            }
        }
    };
    if !status.success() {
        return Err(format!(
            "staged ocm binary failed its version check with status {}",
            status
        ));
    }
    capture.rewind().map_err(|e| e.to_string())?;
    let mut stdout = String::new();
    capture
        .take(1024)
        .read_to_string(&mut stdout)
        .map_err(|_| "staged ocm binary returned a non-UTF-8 version".to_string())?;
    let reported = stdout.strip_suffix('\n').unwrap_or(&stdout);
    let reported = reported.strip_suffix('\r').unwrap_or(reported);
    if reported != target_version {
        return Err(format!(
            "staged ocm binary reported version {reported:?}; expected {target_version:?}"
        ));
    }
    Ok(())
}

fn normalize_release_tag(version: &str) -> String {
    let trimmed = version.trim();
    if trimmed.starts_with('v') {
        trimmed.to_string()
    } else {
        format!("v{trimmed}")
    }
}

fn display_version_from_tag(tag: &str) -> Result<String, String> {
    parse_release_version(tag).map(|version| version.to_string())
}

fn parse_release_version(value: &str) -> Result<Version, String> {
    let normalized = value.trim().strip_prefix('v').unwrap_or(value.trim());
    Version::parse(normalized)
        .map_err(|error| format!("invalid ocm release version \"{value}\": {error}"))
}

fn should_treat_target_as_current(
    current_version: &str,
    target_version: &str,
    explicit_version: bool,
) -> Result<bool, String> {
    let current = parse_release_version(current_version)?;
    let target = parse_release_version(target_version)?;
    Ok(if explicit_version {
        current.to_string() == target.to_string()
    } else {
        current.cmp_precedence(&target).is_ge()
    })
}

#[cfg(test)]
mod tests {
    use super::{
        SelfUpdateTempDir, display_version_from_tag, normalize_release_tag, parse_release_version,
        release_asset_name, self_release_url, should_treat_target_as_current,
    };
    use std::collections::BTreeMap;

    #[test]
    fn normalize_release_tag_accepts_prefixed_and_bare_versions() {
        assert_eq!(normalize_release_tag("0.2.1"), "v0.2.1");
        assert_eq!(normalize_release_tag("v0.2.1"), "v0.2.1");
    }

    #[test]
    fn self_release_urls_use_the_canonical_repository() {
        let env = BTreeMap::new();
        assert_eq!(
            self_release_url(&env, None),
            "https://api.github.com/repos/openclaw/ocm/releases/latest"
        );
        assert_eq!(
            self_release_url(&env, Some("0.2.33")),
            "https://api.github.com/repos/openclaw/ocm/releases/tags/v0.2.33"
        );
    }

    #[test]
    fn self_release_url_override_requires_a_non_empty_value() {
        let mut env = BTreeMap::new();
        env.insert(
            super::INTERNAL_SELF_UPDATE_RELEASE_URL_ENV.to_string(),
            " https://example.test/internal-release.json ".to_string(),
        );
        assert_eq!(
            self_release_url(&env, Some("0.2.33")),
            "https://example.test/internal-release.json"
        );

        env.insert(
            super::INTERNAL_SELF_UPDATE_RELEASE_URL_ENV.to_string(),
            String::new(),
        );
        assert_eq!(
            self_release_url(&env, None),
            "https://api.github.com/repos/openclaw/ocm/releases/latest"
        );

        env.insert(
            super::INTERNAL_SELF_UPDATE_RELEASE_URL_ENV.to_string(),
            " \t\n ".to_string(),
        );
        assert_eq!(
            self_release_url(&env, Some("v0.2.33")),
            "https://api.github.com/repos/openclaw/ocm/releases/tags/v0.2.33"
        );
    }

    #[test]
    fn display_version_from_tag_strips_the_v_prefix() {
        assert_eq!(display_version_from_tag("v0.2.1").unwrap(), "0.2.1");
        assert_eq!(display_version_from_tag("0.2.1").unwrap(), "0.2.1");
    }

    #[test]
    fn release_versions_follow_semver_precedence() {
        assert!(
            parse_release_version("1.0.0-beta.2").unwrap()
                < parse_release_version("1.0.0-beta.10").unwrap()
        );
        assert!(
            parse_release_version("1.0.0-alpha.1").unwrap()
                < parse_release_version("1.0.0-beta.1").unwrap()
        );
        assert_eq!(
            parse_release_version("1.0.0+build.1")
                .unwrap()
                .cmp_precedence(&parse_release_version("1.0.0+build.2").unwrap()),
            std::cmp::Ordering::Equal
        );
        assert!(parse_release_version("1.0.beta").is_err());
    }

    #[test]
    fn should_treat_target_as_current_only_skips_implicit_downgrades() {
        assert!(should_treat_target_as_current("0.2.1", "0.2.0", false).unwrap());
        assert!(!should_treat_target_as_current("0.2.1", "0.2.0", true).unwrap());
        assert!(should_treat_target_as_current("0.2.1", "0.2.1", true).unwrap());
        assert!(!should_treat_target_as_current("1.0.0-beta.2", "1.0.0-beta.10", false).unwrap());
        assert!(should_treat_target_as_current("1.0.0+old", "1.0.0+new", false).unwrap());
        assert!(!should_treat_target_as_current("1.0.0+old", "1.0.0+new", true).unwrap());
    }

    #[test]
    fn self_update_temp_dirs_clean_up_on_drop() {
        let temp = SelfUpdateTempDir::create().unwrap();
        let path = temp.path().to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        std::fs::write(path.join("partial-download"), b"partial").unwrap();
        drop(temp);
        assert!(!path.exists());
    }

    #[test]
    fn self_update_targets_match_the_release_matrix() {
        let workflow = include_str!("../../.github/workflows/release.yml");
        for (os, arch, target) in [
            ("linux", "x86_64", "x86_64-unknown-linux-gnu"),
            ("macos", "x86_64", "x86_64-apple-darwin"),
            ("macos", "aarch64", "aarch64-apple-darwin"),
        ] {
            assert_eq!(
                release_asset_name(os, arch).unwrap(),
                format!("ocm-{target}.tar.gz")
            );
            assert!(workflow.contains(&format!("target: {target}")));
        }
        assert_eq!(
            release_asset_name("linux", "aarch64").unwrap_err(),
            "ocm self update is not supported on this platform yet: aarch64-unknown-linux-gnu"
        );
        assert!(!workflow.contains("target: aarch64-unknown-linux-gnu"));
    }
}
