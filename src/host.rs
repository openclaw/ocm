use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::managed_node::{
    OPENCLAW_MIN_NODE_VERSION, OPENCLAW_NODE_VERSION_REQUIREMENT, managed_node_fallback_detail,
    managed_node_fallback_supported,
};
use crate::service::{ServiceManagerKind, service_manager_kind};
use crate::store::resolve_user_home;

const INTERNAL_NPM_BIN_ENV: &str = "OCM_INTERNAL_NPM_BIN";
const INTERNAL_HOST_PLATFORM_ENV: &str = "OCM_INTERNAL_HOST_PLATFORM";
const INTERNAL_HOST_PACKAGE_MANAGER_ENV: &str = "OCM_INTERNAL_HOST_PACKAGE_MANAGER";
const INTERNAL_HOST_IS_ROOT_ENV: &str = "OCM_INTERNAL_HOST_IS_ROOT";
const CHROME_MCP_MIN_MAJOR: u32 = 144;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostDoctorSummary {
    pub healthy: bool,
    pub official_release_ready: bool,
    pub required_issues: usize,
    pub recommended_gaps: usize,
    pub checks: Vec<HostCheckSummary>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCheckSummary {
    pub category: String,
    pub name: String,
    pub purpose: String,
    pub level: String,
    pub status: String,
    pub available: bool,
    pub version: Option<String>,
    pub detail: Option<String>,
    pub suggestion: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostToolFixSummary {
    pub tool: String,
    pub ready: bool,
    pub changed: bool,
    pub manager: Option<String>,
    pub version: Option<String>,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostCheckLevel {
    Required,
    Recommended,
    Optional,
}

impl HostCheckLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Recommended => "recommended",
            Self::Optional => "optional",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostCheckStatus {
    Ok,
    Missing,
    Outdated,
    Unsupported,
}

impl HostCheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Outdated => "outdated",
            Self::Unsupported => "unsupported",
        }
    }

    fn available(self) -> bool {
        matches!(self, Self::Ok | Self::Outdated)
    }
}

#[derive(Clone, Debug)]
struct HostCommandSummary {
    version: String,
}

#[derive(Clone, Debug, Default)]
struct HostCheckNotes {
    version: Option<String>,
    detail: Option<String>,
    suggestion: Option<String>,
}

impl HostCheckNotes {
    fn new(version: Option<String>, detail: Option<String>, suggestion: Option<String>) -> Self {
        Self {
            version,
            detail,
            suggestion,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostPackageManager {
    Brew,
    AptGet,
    Dnf,
    Yum,
    Apk,
}

impl HostPackageManager {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "brew" => Some(Self::Brew),
            "apt-get" | "apt" => Some(Self::AptGet),
            "dnf" => Some(Self::Dnf),
            "yum" => Some(Self::Yum),
            "apk" => Some(Self::Apk),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Brew => "brew",
            Self::AptGet => "apt-get",
            Self::Dnf => "dnf",
            Self::Yum => "yum",
            Self::Apk => "apk",
        }
    }
}

#[derive(Clone, Debug)]
struct HostInstallCommand {
    program: String,
    args: Vec<String>,
}

impl HostInstallCommand {
    fn new(program: impl Into<String>, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    fn display(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Clone, Debug)]
struct HostToolFixPlan {
    manager: HostPackageManager,
    commands: Vec<HostInstallCommand>,
}

pub fn doctor_host(env: &BTreeMap<String, String>) -> HostDoctorSummary {
    let checks = host_checks(env);
    let required_issues = checks
        .iter()
        .filter(|check| {
            check.level == HostCheckLevel::Required.as_str()
                && check.status != HostCheckStatus::Ok.as_str()
        })
        .count();
    let recommended_gaps = checks
        .iter()
        .filter(|check| {
            check.level == HostCheckLevel::Recommended.as_str()
                && check.status != HostCheckStatus::Ok.as_str()
                && check.status != HostCheckStatus::Unsupported.as_str()
        })
        .count();

    HostDoctorSummary {
        healthy: required_issues == 0,
        official_release_ready: required_issues == 0,
        required_issues,
        recommended_gaps,
        checks,
    }
}

pub fn official_openclaw_runtime_requirement_message(
    detail: &str,
    env: &BTreeMap<String, String>,
) -> String {
    format!(
        "official OpenClaw runtimes require {OPENCLAW_NODE_VERSION_REQUIREMENT} and npm on PATH; {detail}. Run \"{} doctor host\" for a full machine check.",
        command_example(env)
    )
}

pub fn verify_official_openclaw_runtime_node(env: &BTreeMap<String, String>) -> Result<(), String> {
    let node_version = node_version(env)
        .map_err(|detail| official_openclaw_runtime_requirement_message(&detail, env))?;
    match openclaw_node_version_supported(&node_version) {
        Some(true) => Ok(()),
        Some(false) => Err(official_openclaw_runtime_requirement_message(
            &format!(
                "found Node.js {node_version}, which is outside the supported OpenClaw ranges"
            ),
            env,
        )),
        None => Err(official_openclaw_runtime_requirement_message(
            &format!("node --version returned an unreadable version: {node_version}"),
            env,
        )),
    }
}

pub fn verify_official_openclaw_runtime_host(env: &BTreeMap<String, String>) -> Result<(), String> {
    let _ = npm_version(env)
        .map_err(|detail| official_openclaw_runtime_requirement_message(&detail, env))?;
    verify_official_openclaw_runtime_node(env)
}

pub(crate) fn verify_package_runtime_node(
    env: &BTreeMap<String, String>,
    requirement: &crate::managed_node::NodeRequirement,
) -> Result<(), String> {
    let version = node_version(env)?;
    if requirement.supports(&version) {
        Ok(())
    } else {
        Err(format!(
            "OpenClaw package requires Node.js {}; found {version}",
            requirement.description()
        ))
    }
}

pub(crate) fn verify_package_runtime_host(
    env: &BTreeMap<String, String>,
    requirement: &crate::managed_node::NodeRequirement,
) -> Result<(), String> {
    verify_package_runtime_node(env, requirement)?;
    npm_version(env).map(|_| ())
}

pub fn verify_official_openclaw_runtime_support(
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    if verify_official_openclaw_runtime_host(env).is_ok() || managed_node_fallback_supported() {
        return Ok(());
    }

    let fallback_detail = managed_node_fallback_detail().unwrap_or_else(|detail| detail);
    Err(official_openclaw_runtime_requirement_message(
        &format!("{fallback_detail}. Install the host tools on this machine"),
        env,
    ))
}

pub fn compare_version_like(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    let left = parse_version_like(left)?;
    let right = parse_version_like(right)?;
    let max_len = left.len().max(right.len());
    for index in 0..max_len {
        let left_value = *left.get(index).unwrap_or(&0);
        let right_value = *right.get(index).unwrap_or(&0);
        match left_value.cmp(&right_value) {
            std::cmp::Ordering::Equal => {}
            ordering => return Some(ordering),
        }
    }
    Some(std::cmp::Ordering::Equal)
}

pub fn configured_npm_bin(env: &BTreeMap<String, String>) -> &str {
    env.get(INTERNAL_NPM_BIN_ENV)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("npm")
}

pub fn verify_git_host_tool(env: &BTreeMap<String, String>) -> Result<String, String> {
    let _ = env;
    tool_version("git", &["--version"]).map(|summary| summary.version)
}

pub fn git_host_fix_supported(env: &BTreeMap<String, String>) -> bool {
    verify_git_host_tool(env).is_ok() || plan_git_host_install(env).is_ok()
}

pub fn fix_git_host_tool(env: &BTreeMap<String, String>) -> Result<HostToolFixSummary, String> {
    if let Ok(version) = verify_git_host_tool(env) {
        return Ok(HostToolFixSummary {
            tool: "git".to_string(),
            ready: true,
            changed: false,
            manager: None,
            version: Some(version),
            detail: "git is already available on PATH".to_string(),
        });
    }

    let plan = plan_git_host_install(env)?;
    for command in &plan.commands {
        run_host_install_command(command)?;
    }

    let version = verify_git_host_tool(env).map_err(|detail| {
        format!("git install completed, but git is still unavailable: {detail}")
    })?;
    Ok(HostToolFixSummary {
        tool: "git".to_string(),
        ready: true,
        changed: true,
        manager: Some(plan.manager.as_str().to_string()),
        version: Some(version),
        detail: match plan.manager {
            HostPackageManager::Brew => "Installed git with Homebrew.".to_string(),
            HostPackageManager::AptGet => "Installed git with apt-get.".to_string(),
            HostPackageManager::Dnf => "Installed git with dnf.".to_string(),
            HostPackageManager::Yum => "Installed git with yum.".to_string(),
            HostPackageManager::Apk => "Installed git with apk.".to_string(),
        },
    })
}

fn command_example(env: &BTreeMap<String, String>) -> String {
    env.get("OCM_SELF")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ocm")
        .to_string()
}

fn host_checks(env: &BTreeMap<String, String>) -> Vec<HostCheckSummary> {
    let managed_fallback_supported = managed_node_fallback_supported();
    let mut checks = vec![
        node_check(env, managed_fallback_supported),
        npm_check(env, managed_fallback_supported),
    ];

    if cfg!(unix) {
        checks.push(python_check());
    }

    checks.extend([
        ffmpeg_check(),
        ffprobe_check(),
        openssl_check(),
        git_check(env),
        pnpm_check(),
        bun_check(),
        chrome_check(env),
        service_manager_check(env),
    ]);

    checks
}

fn node_check(
    env: &BTreeMap<String, String>,
    managed_fallback_supported: bool,
) -> HostCheckSummary {
    let level = if managed_fallback_supported {
        HostCheckLevel::Recommended
    } else {
        HostCheckLevel::Required
    };
    match node_version(env) {
        Ok(version) => match openclaw_node_version_supported(&version) {
            Some(false) => check(
                "official-release",
                "Node.js",
                "Baseline Node.js availability (package compatibility checked at install and launch)",
                level,
                HostCheckStatus::Outdated,
                HostCheckNotes::new(
                    Some(version.clone()),
                    Some(format!(
                        "found Node.js {version}; legacy packages without engines.node use {OPENCLAW_NODE_VERSION_REQUIREMENT}; selected releases are checked against their own requirement"
                    )),
                    Some(node_suggestion(managed_fallback_supported)),
                ),
            ),
            Some(true) => check(
                "official-release",
                "Node.js",
                "Baseline Node.js availability (package compatibility checked at install and launch)",
                level,
                HostCheckStatus::Ok,
                HostCheckNotes::new(Some(version), Some("Meets the baseline for legacy packages; each selected package is checked against its own engines.node requirement.".to_string()), None),
            ),
            None => check(
                "official-release",
                "Node.js",
                "Baseline Node.js availability (package compatibility checked at install and launch)",
                level,
                HostCheckStatus::Outdated,
                HostCheckNotes::new(
                    Some(version.clone()),
                    Some(format!(
                        "node --version returned an unreadable version: {version}"
                    )),
                    Some(node_suggestion(managed_fallback_supported)),
                ),
            ),
        },
        Err(detail) => check(
            "official-release",
            "Node.js",
            "Baseline Node.js availability (package compatibility checked at install and launch)",
            level,
            HostCheckStatus::Missing,
            HostCheckNotes::new(
                None,
                Some(node_detail(detail, managed_fallback_supported)),
                Some(node_suggestion(managed_fallback_supported)),
            ),
        ),
    }
}

fn npm_check(env: &BTreeMap<String, String>, managed_fallback_supported: bool) -> HostCheckSummary {
    let level = if managed_fallback_supported {
        HostCheckLevel::Recommended
    } else {
        HostCheckLevel::Required
    };
    match npm_version(env) {
        Ok(version) => check(
            "official-release",
            "npm",
            "Install published OpenClaw releases",
            level,
            HostCheckStatus::Ok,
            HostCheckNotes::new(Some(version), None, None),
        ),
        Err(detail) => check(
            "official-release",
            "npm",
            "Install published OpenClaw releases",
            level,
            HostCheckStatus::Missing,
            HostCheckNotes::new(
                None,
                Some(npm_detail(detail, managed_fallback_supported)),
                Some(npm_suggestion(managed_fallback_supported)),
            ),
        ),
    }
}

fn node_detail(detail: String, managed_fallback_supported: bool) -> String {
    if managed_fallback_supported {
        format!(
            "{detail}; OCM can install a private Node.js toolchain for official releases on this platform"
        )
    } else {
        detail
    }
}

fn node_suggestion(managed_fallback_supported: bool) -> String {
    if managed_fallback_supported {
        format!(
            "Install {OPENCLAW_NODE_VERSION_REQUIREMENT} if you want OCM to use your host toolchain; otherwise OCM can manage a private copy for official releases."
        )
    } else {
        format!("Install {OPENCLAW_NODE_VERSION_REQUIREMENT}.")
    }
}

fn npm_detail(detail: String, managed_fallback_supported: bool) -> String {
    if managed_fallback_supported {
        format!(
            "{detail}; OCM can manage a private Node.js + npm toolchain for official releases on this platform"
        )
    } else {
        detail
    }
}

fn npm_suggestion(managed_fallback_supported: bool) -> String {
    if managed_fallback_supported {
        "Install npm if you want OCM to use your host toolchain; otherwise OCM can manage a private copy for official releases.".to_string()
    } else {
        "Install npm or use a Node.js distribution that includes it.".to_string()
    }
}

fn python_check() -> HostCheckSummary {
    match tool_version("python3", &["--version"]) {
        Ok(summary) => check(
            "common-features",
            "python3",
            "Support hardened file operations on macOS/Linux",
            HostCheckLevel::Recommended,
            HostCheckStatus::Ok,
            HostCheckNotes::new(Some(summary.version), None, None),
        ),
        Err(detail) => check(
            "common-features",
            "python3",
            "Support hardened file operations on macOS/Linux",
            HostCheckLevel::Recommended,
            HostCheckStatus::Missing,
            HostCheckNotes::new(
                None,
                Some(detail),
                Some("Install python3 in a system-managed location.".to_string()),
            ),
        ),
    }
}

fn ffmpeg_check() -> HostCheckSummary {
    tool_check(
        "common-features",
        "ffmpeg",
        "Audio and video conversion",
        HostCheckLevel::Recommended,
        "ffmpeg",
        &["-version"],
        Some("Install ffmpeg with your system package manager.".to_string()),
    )
}

fn ffprobe_check() -> HostCheckSummary {
    tool_check(
        "common-features",
        "ffprobe",
        "Media inspection and codec detection",
        HostCheckLevel::Recommended,
        "ffprobe",
        &["-version"],
        Some("Install ffmpeg with your system package manager.".to_string()),
    )
}

fn openssl_check() -> HostCheckSummary {
    tool_check(
        "common-features",
        "openssl",
        "Auto-generate gateway TLS certs",
        HostCheckLevel::Recommended,
        "openssl",
        &["version"],
        Some(
            "Install openssl in a trusted system location if you want gateway TLS auto-generation."
                .to_string(),
        ),
    )
}

fn git_check(env: &BTreeMap<String, String>) -> HostCheckSummary {
    let suggestion = if git_host_fix_supported(env) {
        format!(
            "Run \"{} doctor host --fix git --yes\" to let OCM install git, or install it with your system package manager.",
            command_example(env)
        )
    } else {
        "Install git if you want repo-aware coding workflows, source installs, or git-channel updates.".to_string()
    };
    tool_check(
        "local-workflows",
        "git",
        "Repo-aware coding workflows, source installs, and git-based updates",
        HostCheckLevel::Recommended,
        "git",
        &["--version"],
        Some(suggestion),
    )
}

fn pnpm_check() -> HostCheckSummary {
    tool_check(
        "local-workflows",
        "pnpm",
        "Local OpenClaw checkout workflows",
        HostCheckLevel::Recommended,
        "pnpm",
        &["--version"],
        Some("Install pnpm if you want local OpenClaw checkout workflows.".to_string()),
    )
}

fn bun_check() -> HostCheckSummary {
    tool_check(
        "local-workflows",
        "bun",
        "Optional Bun-based local workflows",
        HostCheckLevel::Optional,
        "bun",
        &["--version"],
        Some("Install bun only if you plan to run Bun-based local commands.".to_string()),
    )
}

fn chrome_check(env: &BTreeMap<String, String>) -> HostCheckSummary {
    let Some(path) = resolve_google_chrome_executable_for_platform(env, std::env::consts::OS)
    else {
        return check(
            "browser",
            "Google Chrome",
            "Chrome MCP existing-session browser flows",
            HostCheckLevel::Recommended,
            HostCheckStatus::Missing,
            HostCheckNotes::new(
                None,
                Some(format!(
                    "Google Chrome {CHROME_MCP_MIN_MAJOR}+ was not found on this host"
                )),
                Some(format!(
                    "Install Google Chrome {CHROME_MCP_MIN_MAJOR}+ if you want Chrome MCP existing-session flows."
                )),
            ),
        );
    };

    match tool_version(path.to_string_lossy().as_ref(), &["--version"]) {
        Ok(summary) => match parse_browser_major_version(&summary.version) {
            Some(major) if major < CHROME_MCP_MIN_MAJOR => check(
                "browser",
                "Google Chrome",
                "Chrome MCP existing-session browser flows",
                HostCheckLevel::Recommended,
                HostCheckStatus::Outdated,
                HostCheckNotes::new(
                    Some(summary.version.clone()),
                    Some(format!(
                        "detected Chrome {summary}; upgrade to {CHROME_MCP_MIN_MAJOR}+",
                        summary = summary.version
                    )),
                    Some(format!(
                        "Upgrade Google Chrome to {CHROME_MCP_MIN_MAJOR} or newer for Chrome MCP existing-session flows."
                    )),
                ),
            ),
            Some(_) => check(
                "browser",
                "Google Chrome",
                "Chrome MCP existing-session browser flows",
                HostCheckLevel::Recommended,
                HostCheckStatus::Ok,
                HostCheckNotes::new(
                    Some(summary.version),
                    Some(format!("path: {}", path.display())),
                    None,
                ),
            ),
            None => check(
                "browser",
                "Google Chrome",
                "Chrome MCP existing-session browser flows",
                HostCheckLevel::Recommended,
                HostCheckStatus::Outdated,
                HostCheckNotes::new(
                    Some(summary.version.clone()),
                    Some(format!(
                        "could not read the Chrome major version from {}",
                        summary.version
                    )),
                    Some(format!(
                        "Verify Google Chrome is {CHROME_MCP_MIN_MAJOR}+ if you want Chrome MCP existing-session flows."
                    )),
                ),
            ),
        },
        Err(detail) => check(
            "browser",
            "Google Chrome",
            "Chrome MCP existing-session browser flows",
            HostCheckLevel::Recommended,
            HostCheckStatus::Missing,
            HostCheckNotes::new(
                None,
                Some(format!("{} ({detail})", path.display())),
                Some(format!(
                    "Install Google Chrome {CHROME_MCP_MIN_MAJOR}+ if you want Chrome MCP existing-session flows."
                )),
            ),
        ),
    }
}

fn service_manager_check(env: &BTreeMap<String, String>) -> HostCheckSummary {
    match service_manager_kind(env) {
        ServiceManagerKind::Launchd => match tool_version("launchctl", &["help"]) {
            Ok(_) => check(
                "background-services",
                "launchd",
                "Keep envs running in the background",
                HostCheckLevel::Recommended,
                HostCheckStatus::Ok,
                HostCheckNotes::new(None, Some("launchd is available".to_string()), None),
            ),
            Err(detail) => check(
                "background-services",
                "launchd",
                "Keep envs running in the background",
                HostCheckLevel::Recommended,
                HostCheckStatus::Missing,
                HostCheckNotes::new(
                    None,
                    Some(detail),
                    Some(
                        "Use --no-service or make sure launchd is available on this machine."
                            .to_string(),
                    ),
                ),
            ),
        },
        ServiceManagerKind::SystemdUser => match tool_version("systemctl", &["--version"]) {
            Ok(summary) => check(
                "background-services",
                "systemd --user",
                "Keep envs running in the background",
                HostCheckLevel::Recommended,
                HostCheckStatus::Ok,
                HostCheckNotes::new(
                    Some(summary.version),
                    Some("systemd --user service support is available".to_string()),
                    None,
                ),
            ),
            Err(detail) => check(
                "background-services",
                "systemd --user",
                "Keep envs running in the background",
                HostCheckLevel::Recommended,
                HostCheckStatus::Missing,
                HostCheckNotes::new(
                    None,
                    Some(detail),
                    Some(
                        "Use --no-service or install systemd user services on this machine."
                            .to_string(),
                    ),
                ),
            ),
        },
        ServiceManagerKind::Unsupported => check(
            "background-services",
            "managed services",
            "Keep envs running in the background",
            HostCheckLevel::Recommended,
            HostCheckStatus::Missing,
            HostCheckNotes::new(
                None,
                Some("managed services are not supported on this platform yet".to_string()),
                Some(
                    "Use --no-service and run OpenClaw directly inside the env for now."
                        .to_string(),
                ),
            ),
        ),
    }
}

fn tool_check(
    category: &str,
    name: &str,
    purpose: &str,
    level: HostCheckLevel,
    command: &str,
    args: &[&str],
    suggestion: Option<String>,
) -> HostCheckSummary {
    match tool_version(command, args) {
        Ok(summary) => check(
            category,
            name,
            purpose,
            level,
            HostCheckStatus::Ok,
            HostCheckNotes::new(Some(summary.version), None, None),
        ),
        Err(detail) => check(
            category,
            name,
            purpose,
            level,
            HostCheckStatus::Missing,
            HostCheckNotes::new(None, Some(detail), suggestion),
        ),
    }
}

fn plan_git_host_install(env: &BTreeMap<String, String>) -> Result<HostToolFixPlan, String> {
    match host_platform(env) {
        "macos" => plan_macos_git_install(env),
        "linux" => plan_linux_git_install(env),
        "windows" => Err(
            "OCM cannot install git automatically on Windows yet; install git manually and rerun your workflow."
                .to_string(),
        ),
        platform => Err(format!(
            "OCM does not know how to install git automatically on this platform ({platform})."
        )),
    }
}

fn plan_macos_git_install(env: &BTreeMap<String, String>) -> Result<HostToolFixPlan, String> {
    let manager = detect_host_package_manager(env);
    if manager != Some(HostPackageManager::Brew) {
        return Err(
            "Homebrew is not installed. OCM will not install Homebrew automatically; install git manually or install Homebrew first."
                .to_string(),
        );
    }

    Ok(HostToolFixPlan {
        manager: HostPackageManager::Brew,
        commands: vec![HostInstallCommand::new("brew", ["install", "git"])],
    })
}

fn plan_linux_git_install(env: &BTreeMap<String, String>) -> Result<HostToolFixPlan, String> {
    let Some(manager) = detect_host_package_manager(env) else {
        return Err(
            "No supported Linux package manager was found. OCM currently supports apt-get, dnf, yum, and apk for automatic git installs."
                .to_string(),
        );
    };
    let commands = match manager {
        HostPackageManager::AptGet => vec![
            privileged_install_command(env, "apt-get", ["update"])?,
            privileged_install_command(env, "apt-get", ["install", "-y", "git"])?,
        ],
        HostPackageManager::Dnf => {
            vec![privileged_install_command(
                env,
                "dnf",
                ["install", "-y", "git"],
            )?]
        }
        HostPackageManager::Yum => {
            vec![privileged_install_command(
                env,
                "yum",
                ["install", "-y", "git"],
            )?]
        }
        HostPackageManager::Apk => {
            vec![privileged_install_command(env, "apk", ["add", "git"])?]
        }
        HostPackageManager::Brew => {
            return plan_macos_git_install(env);
        }
    };

    Ok(HostToolFixPlan { manager, commands })
}

fn host_platform(env: &BTreeMap<String, String>) -> &str {
    env.get(INTERNAL_HOST_PLATFORM_ENV)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(std::env::consts::OS)
}

fn detect_host_package_manager(env: &BTreeMap<String, String>) -> Option<HostPackageManager> {
    if let Some(value) = env.get(INTERNAL_HOST_PACKAGE_MANAGER_ENV) {
        return HostPackageManager::parse(value);
    }

    match host_platform(env) {
        "macos" => command_exists("brew").then_some(HostPackageManager::Brew),
        "linux" => [
            HostPackageManager::AptGet,
            HostPackageManager::Dnf,
            HostPackageManager::Yum,
            HostPackageManager::Apk,
        ]
        .into_iter()
        .find(|manager| command_exists(manager.as_str())),
        _ => None,
    }
}

fn privileged_install_command(
    env: &BTreeMap<String, String>,
    program: &str,
    args: impl IntoIterator<Item = impl Into<String>>,
) -> Result<HostInstallCommand, String> {
    let args = args.into_iter().map(Into::into).collect::<Vec<String>>();
    if host_is_root(env) {
        return Ok(HostInstallCommand::new(program, args));
    }
    if command_exists("sudo") {
        let mut sudo_args = vec![program.to_string()];
        sudo_args.extend(args);
        return Ok(HostInstallCommand::new("sudo", sudo_args));
    }

    Err(
        "Installing git requires elevated privileges on Linux. Run as root or install sudo, then retry."
            .to_string(),
    )
}

fn host_is_root(env: &BTreeMap<String, String>) -> bool {
    if let Some(value) = env.get(INTERNAL_HOST_IS_ROOT_ENV) {
        let value = value.trim().to_ascii_lowercase();
        return matches!(value.as_str(), "1" | "true" | "yes" | "root");
    }

    env.get("USER").is_some_and(|value| value == "root")
        || env.get("HOME").is_some_and(|value| value == "/root")
}

fn command_exists(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn run_host_install_command(command: &HostInstallCommand) -> Result<(), String> {
    let output = Command::new(&command.program)
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("{}: {}", command.display(), error))?;
    if output.status.success() {
        return Ok(());
    }

    let detail = summarize_error_output(&output.stderr, &output.stdout)
        .unwrap_or_else(|| format!("exited with code {}", output.status.code().unwrap_or(1)));
    Err(format!("{} failed: {detail}", command.display()))
}

fn check(
    category: &str,
    name: &str,
    purpose: &str,
    level: HostCheckLevel,
    status: HostCheckStatus,
    notes: HostCheckNotes,
) -> HostCheckSummary {
    HostCheckSummary {
        category: category.to_string(),
        name: name.to_string(),
        purpose: purpose.to_string(),
        level: level.as_str().to_string(),
        status: status.as_str().to_string(),
        available: status.available(),
        version: notes.version,
        detail: notes.detail,
        suggestion: notes.suggestion,
    }
}

fn npm_version(env: &BTreeMap<String, String>) -> Result<String, String> {
    tool_version(configured_npm_bin(env), &["--version"]).map(|summary| summary.version)
}

fn node_version(env: &BTreeMap<String, String>) -> Result<String, String> {
    let _ = env;
    tool_version("node", &["--version"]).map(|summary| summary.version)
}

fn tool_version(command: &str, args: &[&str]) -> Result<HostCommandSummary, String> {
    let output = Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("{command} {}", command_failure_detail(error)))?;
    if !output.status.success() {
        let detail = summarize_error_output(&output.stderr, &output.stdout).unwrap_or_else(|| {
            format!(
                "{command} exited with code {}",
                output.status.code().unwrap_or(1)
            )
        });
        return Err(format!("{command} could not be run: {detail}"));
    }

    let version = summarize_command_output(&output.stdout, &output.stderr).unwrap_or_else(|| {
        args.first()
            .map(|arg| format!("{command} {arg} succeeded"))
            .unwrap_or_else(|| format!("{command} is available"))
    });
    Ok(HostCommandSummary { version })
}

fn command_failure_detail(error: std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "was not found on PATH".to_string(),
        _ => error.to_string(),
    }
}

fn summarize_command_output(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    for bytes in [stdout, stderr] {
        let text = String::from_utf8_lossy(bytes);
        if let Some(line) = text.lines().find_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then_some(trimmed.to_string())
        }) {
            return Some(line);
        }
    }
    None
}

fn summarize_error_output(stderr: &[u8], stdout: &[u8]) -> Option<String> {
    for bytes in [stderr, stdout] {
        let text = String::from_utf8_lossy(bytes);
        if let Some(line) = text.lines().find_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then_some(trimmed.to_string())
        }) {
            return Some(line);
        }
    }
    None
}

fn parse_version_like(version: &str) -> Option<Vec<u64>> {
    let trimmed = version.trim();
    let trimmed = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let mut out = Vec::new();
    for part in trimmed.split('.') {
        if part.is_empty() {
            return None;
        }
        out.push(part.parse::<u64>().ok()?);
    }
    Some(out)
}

pub(crate) fn openclaw_node_version_supported(version: &str) -> Option<bool> {
    let parsed = parse_version_like(version)?;
    let major = *parsed.first()?;
    let minimum = match major {
        22 => OPENCLAW_MIN_NODE_VERSION,
        23 => return Some(false),
        24 => "24.15.0",
        25 => "25.9.0",
        value if value > 25 => return Some(true),
        _ => return Some(false),
    };
    Some(compare_version_like(version, minimum)? != std::cmp::Ordering::Less)
}

fn parse_browser_major_version(raw_version: &str) -> Option<u32> {
    raw_version
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.'))
        .filter(|token| token.contains('.'))
        .find_map(|token| {
            token.split('.').next().and_then(|major| {
                (!major.is_empty() && major.chars().all(|ch| ch.is_ascii_digit()))
                    .then(|| major.parse::<u32>().ok())
                    .flatten()
            })
        })
}

fn resolve_google_chrome_executable_for_platform(
    env: &BTreeMap<String, String>,
    platform: &str,
) -> Option<PathBuf> {
    match platform {
        "macos" => find_first_existing(&[
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            resolve_user_home(env)
                .join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            PathBuf::from(
                "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            ),
            resolve_user_home(env)
                .join("Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary"),
        ]),
        "linux" => find_first_existing(&[
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/google-chrome-stable"),
            PathBuf::from("/usr/bin/google-chrome-beta"),
            PathBuf::from("/usr/bin/google-chrome-unstable"),
            PathBuf::from("/snap/bin/google-chrome"),
        ]),
        "windows" => {
            let local_app_data = env
                .get("LOCALAPPDATA")
                .cloned()
                .unwrap_or_default()
                .trim()
                .to_string();
            let program_files = env
                .get("ProgramFiles")
                .cloned()
                .unwrap_or_else(|| "C:\\Program Files".to_string());
            let program_files_x86 = env
                .get("ProgramFiles(x86)")
                .cloned()
                .unwrap_or_else(|| "C:\\Program Files (x86)".to_string());
            let mut candidates = Vec::new();
            if !local_app_data.is_empty() {
                candidates.push(
                    PathBuf::from(local_app_data.clone())
                        .join("Google/Chrome/Application/chrome.exe"),
                );
                candidates.push(
                    PathBuf::from(local_app_data).join("Google/Chrome SxS/Application/chrome.exe"),
                );
            }
            candidates
                .push(PathBuf::from(program_files).join("Google/Chrome/Application/chrome.exe"));
            candidates.push(
                PathBuf::from(program_files_x86).join("Google/Chrome/Application/chrome.exe"),
            );
            find_first_existing(&candidates)
        }
        _ => None,
    }
}

fn find_first_existing(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|path| path.exists()).cloned()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        HostPackageManager, command_exists, compare_version_like, detect_host_package_manager,
        host_is_root, openclaw_node_version_supported, parse_browser_major_version,
    };

    #[test]
    fn compare_version_like_orders_semverish_versions() {
        assert_eq!(
            compare_version_like("22.22.3", "22.22.3"),
            Some(std::cmp::Ordering::Equal)
        );
        assert_eq!(
            compare_version_like("v22.23.1", "22.22.3"),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            compare_version_like("20.11.0", "22.22.3"),
            Some(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn openclaw_node_version_support_matches_current_runtime_lines() {
        for version in ["22.22.3", "22.23.0", "24.15.0", "25.9.0", "26.0.0"] {
            assert_eq!(
                openclaw_node_version_supported(version),
                Some(true),
                "{version}"
            );
        }
        for version in ["20.20.0", "22.22.2", "23.11.0", "24.14.9", "25.8.9"] {
            assert_eq!(
                openclaw_node_version_supported(version),
                Some(false),
                "{version}"
            );
        }
        assert_eq!(openclaw_node_version_supported("unknown"), None);
    }

    #[test]
    fn parse_browser_major_version_extracts_chrome_major() {
        assert_eq!(
            parse_browser_major_version("Google Chrome 144.0.7540.0"),
            Some(144)
        );
        assert_eq!(
            parse_browser_major_version("Google Chrome Canary 145.1.2.3"),
            Some(145)
        );
        assert_eq!(parse_browser_major_version("unknown"), None);
    }

    #[test]
    fn detect_host_package_manager_respects_internal_override() {
        let mut env = BTreeMap::new();
        env.insert(
            "OCM_INTERNAL_HOST_PLATFORM".to_string(),
            "linux".to_string(),
        );
        env.insert(
            "OCM_INTERNAL_HOST_PACKAGE_MANAGER".to_string(),
            "apt-get".to_string(),
        );

        assert_eq!(
            detect_host_package_manager(&env),
            Some(HostPackageManager::AptGet)
        );
    }

    #[test]
    fn host_is_root_respects_internal_override() {
        let mut env = BTreeMap::new();
        env.insert("OCM_INTERNAL_HOST_IS_ROOT".to_string(), "true".to_string());
        assert!(host_is_root(&env));

        env.insert("OCM_INTERNAL_HOST_IS_ROOT".to_string(), "false".to_string());
        assert!(!host_is_root(&env));
    }

    #[test]
    fn command_exists_returns_false_for_missing_commands() {
        assert!(!command_exists("ocm-test-command-that-should-not-exist"));
    }
}
