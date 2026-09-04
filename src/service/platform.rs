use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::store::{display_path, lock_file, resolve_user_home};

pub(crate) const OCM_SERVICE_LABEL: &str = "ai.openclaw.ocm";
const SERVICE_MANAGER_OVERRIDE: &str = "OCM_INTERNAL_SERVICE_MANAGER";
const LAUNCHCTL_BIN_OVERRIDE: &str = "OCM_INTERNAL_LAUNCHCTL_BIN";
const SYSTEMCTL_BIN_OVERRIDE: &str = "OCM_INTERNAL_SYSTEMCTL_BIN";
const ID_BIN_OVERRIDE: &str = "OCM_INTERNAL_ID_BIN";
const SERVICE_DIR_MODE: u32 = 0o700;
const SERVICE_FILE_MODE: u32 = 0o600;
const LAUNCH_AGENT_THROTTLE_INTERVAL_SECONDS: u32 = 1;
const LAUNCH_AGENT_UMASK_DECIMAL: u32 = 0o077;
const SERVICE_MANAGER_BUSY_ATTEMPTS: usize = 3;
const SERVICE_MANAGER_BUSY_RETRY_MS: u64 = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedServiceIdentity {
    pub(crate) label: String,
    pub(crate) definition_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedServiceDefinition {
    pub(crate) label: String,
    pub(crate) description: String,
    pub(crate) definition_path: PathBuf,
    pub(crate) program_arguments: Vec<String>,
    pub(crate) working_directory: PathBuf,
    pub(crate) stdout_path: PathBuf,
    pub(crate) stderr_path: PathBuf,
    pub(crate) environment: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServiceManagerKind {
    Launchd,
    SystemdUser,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagedServiceEnablement {
    Enabled,
    EnabledRuntime,
    Disabled,
}

pub(crate) fn unsupported_service_manager_message() -> &'static str {
    "managed services are not supported on this platform yet; run OpenClaw directly inside the env for now"
}

pub(crate) fn service_backend_support_error(env: &BTreeMap<String, String>) -> Option<String> {
    match service_manager_kind(env) {
        ServiceManagerKind::Launchd => {
            let binary = service_manager_binary(env, ServiceManagerKind::Launchd);
            if !binary_available(binary, env) {
                Some(
                    "managed services require launchctl on this machine; run OpenClaw directly inside the env for now"
                        .to_string(),
                )
            } else if launchd_available(env) {
                None
            } else {
                Some(
                    "managed services require a usable launchctl session on this machine; run OpenClaw directly inside the env for now"
                        .to_string(),
                )
            }
        }
        ServiceManagerKind::SystemdUser => {
            let binary = service_manager_binary(env, ServiceManagerKind::SystemdUser);
            if !binary_available(binary, env) {
                Some(
                    "managed services require systemctl --user on this machine; run OpenClaw directly inside the env for now"
                        .to_string(),
                )
            } else if systemd_user_available(env) {
                None
            } else {
                Some(
                    "managed services require a usable systemctl --user session on this machine; run OpenClaw directly inside the env for now"
                        .to_string(),
                )
            }
        }
        ServiceManagerKind::Unsupported => Some(unsupported_service_manager_message().to_string()),
    }
}

pub(crate) fn service_manager_kind(env: &BTreeMap<String, String>) -> ServiceManagerKind {
    if let Some(value) = env.get(SERVICE_MANAGER_OVERRIDE) {
        match value.trim().to_ascii_lowercase().as_str() {
            "launchd" => return ServiceManagerKind::Launchd,
            "systemd" | "systemd-user" | "systemd_user" => {
                return ServiceManagerKind::SystemdUser;
            }
            "unsupported" => return ServiceManagerKind::Unsupported,
            _ => {}
        }
    }

    if cfg!(target_os = "linux") {
        ServiceManagerKind::SystemdUser
    } else if cfg!(target_os = "macos") {
        ServiceManagerKind::Launchd
    } else {
        ServiceManagerKind::Unsupported
    }
}

fn service_manager_binary(env: &BTreeMap<String, String>, kind: ServiceManagerKind) -> &str {
    match kind {
        ServiceManagerKind::Launchd => env
            .get(LAUNCHCTL_BIN_OVERRIDE)
            .map(String::as_str)
            .unwrap_or("launchctl"),
        ServiceManagerKind::SystemdUser => env
            .get(SYSTEMCTL_BIN_OVERRIDE)
            .map(String::as_str)
            .unwrap_or("systemctl"),
        ServiceManagerKind::Unsupported => "",
    }
}

fn binary_available(binary: &str, env: &BTreeMap<String, String>) -> bool {
    if binary.trim().is_empty() {
        return false;
    }
    let path = Path::new(binary);
    if path.is_absolute() || binary.contains(std::path::MAIN_SEPARATOR) {
        return path.is_file();
    }
    let Some(path_value) = env.get("PATH") else {
        return false;
    };
    std::env::split_paths(path_value)
        .map(|dir| dir.join(binary))
        .any(|candidate| candidate.is_file())
}

fn launchd_available(env: &BTreeMap<String, String>) -> bool {
    Command::new(service_manager_binary(env, ServiceManagerKind::Launchd))
        .arg("managername")
        .envs(env)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn systemd_user_available(env: &BTreeMap<String, String>) -> bool {
    Command::new(service_manager_binary(env, ServiceManagerKind::SystemdUser))
        .args(["--user", "show-environment"])
        .envs(env)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(crate) fn managed_service_label() -> &'static str {
    OCM_SERVICE_LABEL
}

pub(crate) fn managed_service_identity(
    env: &BTreeMap<String, String>,
    _cwd: &Path,
) -> Result<ManagedServiceIdentity, String> {
    let label = managed_service_label().to_string();
    Ok(ManagedServiceIdentity {
        definition_path: service_definition_dir(env).join(format!(
            "{}.{}",
            label,
            service_definition_extension(service_manager_kind(env))
        )),
        label,
    })
}

pub(crate) fn service_definition_dir(env: &BTreeMap<String, String>) -> PathBuf {
    let home = resolve_user_home(env);
    match service_manager_kind(env) {
        ServiceManagerKind::Launchd => home.join("Library").join("LaunchAgents"),
        ServiceManagerKind::SystemdUser => home.join(".config").join("systemd").join("user"),
        ServiceManagerKind::Unsupported => home.join(".ocm").join("unsupported-services"),
    }
}

pub(crate) fn service_definition_extension(kind: ServiceManagerKind) -> &'static str {
    match kind {
        ServiceManagerKind::Launchd => "plist",
        ServiceManagerKind::SystemdUser => "service",
        ServiceManagerKind::Unsupported => "service",
    }
}

pub(crate) fn write_managed_service_definition(
    definition: &ManagedServiceDefinition,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    ensure_private_dir(
        &definition
            .stdout_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                format!(
                    "failed to resolve stdout log directory for {}",
                    display_path(&definition.stdout_path)
                )
            })?,
    )?;
    ensure_private_dir(
        &definition
            .stderr_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                format!(
                    "failed to resolve stderr log directory for {}",
                    display_path(&definition.stderr_path)
                )
            })?,
    )?;
    let Some(parent) = definition.definition_path.parent() else {
        return Err(format!(
            "failed to resolve service definition directory for {}",
            display_path(&definition.definition_path)
        ));
    };
    ensure_service_definition_dir(parent)?;
    let lock_path = definition.definition_path.with_extension("lock");
    let _lock = lock_file(&lock_path, "managed service definition")?;
    validate_managed_service_owner(definition, env)?;

    let raw = match service_manager_kind(env) {
        ServiceManagerKind::Launchd => build_launch_agent_plist(
            &definition.label,
            &definition.description,
            &definition.program_arguments,
            &definition.working_directory,
            &definition.stdout_path,
            &definition.stderr_path,
            &definition.environment,
        ),
        ServiceManagerKind::SystemdUser => build_systemd_unit(
            &definition.description,
            &definition.program_arguments,
            &definition.working_directory,
            &definition.stdout_path,
            &definition.stderr_path,
            &definition.environment,
            systemd_output_mode(env),
        )?,
        ServiceManagerKind::Unsupported => {
            return Err(unsupported_service_manager_message().to_string());
        }
    };
    write_private_service_file(&definition.definition_path, raw.as_bytes())
}

pub(crate) fn validate_managed_service_owner(
    definition: &ManagedServiceDefinition,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    if !definition.definition_path.exists() {
        return Ok(());
    }
    let Some(ocm_home) = definition.environment.get("OCM_HOME") else {
        return Ok(());
    };
    let raw = fs::read_to_string(&definition.definition_path).map_err(|error| {
        format!(
            "failed to read existing service definition {}: {error}",
            display_path(&definition.definition_path)
        )
    })?;
    let owner_markers = match service_manager_kind(env) {
        ServiceManagerKind::Launchd => vec![format!(
            "<key>OCM_HOME</key>\n      <string>{}</string>",
            plist_escape(ocm_home)
        )],
        ServiceManagerKind::SystemdUser => vec![
            format!("Environment=\"OCM_HOME={}\"", systemd_escape(ocm_home)),
            format!(
                "Environment=\"OCM_HOME={}\"",
                systemd_legacy_escape(ocm_home)
            ),
        ],
        ServiceManagerKind::Unsupported => return Ok(()),
    };
    if owner_markers.iter().any(|marker| raw.contains(marker)) {
        return Ok(());
    }
    Err(format!(
        "the managed OCM service at {} is already bound to a different OCM_HOME; stop and uninstall that service before activating this store",
        display_path(&definition.definition_path)
    ))
}

pub(crate) fn validate_managed_service_executable(
    definition: &ManagedServiceDefinition,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    if !definition.definition_path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(&definition.definition_path).map_err(|e| e.to_string())?;
    let marker = match service_manager_kind(env) {
        ServiceManagerKind::Launchd => format!(
            "<key>ProgramArguments</key>\n    <array>\n      <string>{}</string>",
            plist_escape(&definition.program_arguments[0])
        ),
        ServiceManagerKind::SystemdUser => format!(
            "\nExecStart={} __daemon run\n",
            systemd_quote(&definition.program_arguments[0])
        ),
        ServiceManagerKind::Unsupported => return Err(unsupported_service_manager_message().into()),
    };
    if !raw.contains(&marker) {
        return Err("managed daemon uses a different executable or a nonstandard service definition; refusing self-update".into());
    }
    Ok(())
}

pub(crate) fn deactivate_managed_service(
    definition: &ManagedServiceDefinition,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let definition_path = &definition.definition_path;
    let lock_path = definition_path.with_extension("lock");
    let _lock = lock_file(&lock_path, "managed service definition")?;
    validate_managed_service_owner(definition, env)?;
    if !definition_path.exists() {
        // A completed teardown removes the ownership marker. Repeated stop or
        // uninstall calls must not probe or mutate an unowned manager unit.
        return Ok(());
    }
    match service_manager_kind(env) {
        ServiceManagerKind::Launchd => {
            let domain = gui_domain(env)?;
            deactivate_launchd_service(&definition.label, definition_path, &domain, env)?;
        }
        ServiceManagerKind::SystemdUser => {
            let stop = run_systemctl(env, ["--user", "stop", definition.label.as_str()])?;
            if !stop.status.success() {
                return Err(format!(
                    "systemctl --user stop failed: {}",
                    systemctl_detail(&stop)
                ));
            }
            let disable = run_systemctl(env, ["--user", "disable", definition.label.as_str()])?;
            if !disable.status.success() {
                return Err(format!(
                    "systemctl --user disable failed: {}",
                    systemctl_detail(&disable)
                ));
            }
        }
        ServiceManagerKind::Unsupported => {}
    }

    match fs::remove_file(definition_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to remove managed service definition {}: {error}",
                display_path(definition_path)
            ));
        }
    }

    if service_manager_kind(env) == ServiceManagerKind::SystemdUser {
        let reload = run_systemctl(env, ["--user", "daemon-reload"])?;
        if !reload.status.success() {
            return Err(format!(
                "systemctl --user daemon-reload failed: {}",
                systemctl_detail(&reload)
            ));
        }
    }
    Ok(())
}

fn deactivate_launchd_service(
    label: &str,
    definition_path: &Path,
    domain: &str,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let target = format!("{domain}/{label}");
    if !launchd_job_is_loaded(&target, env)? {
        return Ok(());
    }

    let definition_path = display_path(definition_path);
    let mut failures = Vec::new();
    let output = run_launchctl(env, ["bootout", target.as_str()])?;
    if !launchd_job_is_loaded(&target, env)? {
        return Ok(());
    }
    failures.push(launchctl_detail(&output));
    let output = run_launchctl(env, ["bootout", domain, definition_path.as_str()])?;
    if !launchd_job_is_loaded(&target, env)? {
        return Ok(());
    }
    failures.push(launchctl_detail(&output));
    let output = run_launchctl(env, ["unload", definition_path.as_str()])?;
    if !launchd_job_is_loaded(&target, env)? {
        return Ok(());
    }
    failures.push(launchctl_detail(&output));

    let details = failures
        .into_iter()
        .filter(|detail| !detail.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    Err(if details.is_empty() {
        format!("launchctl failed to unload {target}")
    } else {
        format!("launchctl failed to unload {target}: {details}")
    })
}

fn launchd_job_is_loaded(target: &str, env: &BTreeMap<String, String>) -> Result<bool, String> {
    let output = run_launchctl(env, ["print", target])?;
    if output.status.success() {
        return Ok(true);
    }
    let detail = launchctl_detail(&output);
    if detail
        .to_ascii_lowercase()
        .contains("could not find service")
    {
        return Ok(false);
    }
    Err(if detail.is_empty() {
        format!("launchctl print failed for {target}")
    } else {
        format!("launchctl print failed for {target}: {detail}")
    })
}

pub(crate) fn activate_managed_service(
    label: &str,
    definition_path: &Path,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    match service_manager_kind(env) {
        ServiceManagerKind::Launchd => activate_launchd_service(label, definition_path, env),
        ServiceManagerKind::SystemdUser => activate_systemd_user_service(label, env),
        ServiceManagerKind::Unsupported => Err(unsupported_service_manager_message().to_string()),
    }
}

pub(crate) fn restore_managed_service_registration(
    label: &str,
    definition_path: &Path,
    running: bool,
    enablement: Option<ManagedServiceEnablement>,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    match service_manager_kind(env) {
        ServiceManagerKind::Launchd => {
            restore_launchd_service_registration(label, definition_path, running, enablement, env)
        }
        ServiceManagerKind::SystemdUser => {
            restore_systemd_user_service_registration(label, running, enablement, env)
        }
        ServiceManagerKind::Unsupported => Err(unsupported_service_manager_message().to_string()),
    }
}

pub(crate) fn managed_service_enablement(
    label: &str,
    env: &BTreeMap<String, String>,
) -> Result<Option<ManagedServiceEnablement>, String> {
    if service_manager_kind(env) != ServiceManagerKind::Launchd {
        return Ok(None);
    }
    let domain = gui_domain(env)?;
    let output = run_launchctl(env, ["print-disabled", domain.as_str()])?;
    if !output.status.success() {
        return Err(format!(
            "launchctl print-disabled failed for {domain}: {}",
            launchctl_detail(&output)
        ));
    }
    let disabled = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once("=>"))
        .any(|(key, value)| {
            key.trim().trim_matches('"') == label && value.trim().starts_with("true")
        });
    Ok(Some(if disabled {
        ManagedServiceEnablement::Disabled
    } else {
        ManagedServiceEnablement::Enabled
    }))
}

fn restore_launchd_service_registration(
    label: &str,
    definition_path: &Path,
    running: bool,
    enablement: Option<ManagedServiceEnablement>,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let previous_enablement =
        enablement.ok_or_else(|| format!("failed to capture the enabled state for {label}"))?;
    let domain = gui_domain(env)?;
    let target = format!("{domain}/{label}");
    set_launchd_service_enablement(&target, ManagedServiceEnablement::Enabled, env)?;
    if let Err(error) = activate_launchd_service(label, definition_path, env) {
        return match set_launchd_service_enablement(&target, previous_enablement, env) {
            Ok(()) => Err(error),
            Err(enablement_error) => Err(format!(
                "{error}; failed to restore the previous launchd enabled state: {enablement_error}"
            )),
        };
    }
    if running {
        return set_launchd_service_enablement(&target, previous_enablement, env);
    }

    set_launchd_service_enablement(&target, ManagedServiceEnablement::Disabled, env)?;
    let stop_result = stop_launchd_job_for_restore(&target, env);
    let enablement_result = set_launchd_service_enablement(&target, previous_enablement, env);
    match (stop_result, enablement_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(enablement_error)) => Err(format!(
            "{error}; failed to restore the previous launchd enabled state: {enablement_error}"
        )),
    }
}

fn stop_launchd_job_for_restore(
    target: &str,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    if !launchd_job_is_running(target, env)? {
        return Ok(());
    }
    let terminate = run_launchctl(env, ["kill", "SIGTERM", target])?;
    if !terminate.status.success() {
        return Err(format!(
            "launchctl kill failed while restoring {target}: {}",
            launchctl_detail(&terminate)
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if !launchd_job_is_running(target, env)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "launchctl kept {target} running while restoring its stopped state"
            ));
        }
        sleep(Duration::from_millis(25));
    }
}

fn set_launchd_service_enablement(
    target: &str,
    enablement: ManagedServiceEnablement,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let action = match enablement {
        ManagedServiceEnablement::Enabled => "enable",
        ManagedServiceEnablement::Disabled => "disable",
        ManagedServiceEnablement::EnabledRuntime => {
            return Err("launchd does not support runtime-only enablement".to_string());
        }
    };
    let output = run_launchctl(env, [action, target])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "launchctl {action} failed while restoring {target}: {}",
            launchctl_detail(&output)
        ))
    }
}

fn launchd_job_is_running(target: &str, env: &BTreeMap<String, String>) -> Result<bool, String> {
    let output = run_launchctl(env, ["print", target])?;
    if !output.status.success() {
        return Err(format!(
            "launchctl lost {target} while restoring its stopped state: {}",
            launchctl_detail(&output)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .any(|line| matches!(line, "state = running" | "state = active")))
}

fn build_launch_agent_plist(
    label: &str,
    comment: &str,
    program_arguments: &[String],
    working_directory: &Path,
    stdout_path: &Path,
    stderr_path: &Path,
    environment: &BTreeMap<String, String>,
) -> String {
    let args_xml = program_arguments
        .iter()
        .map(|arg| format!("\n      <string>{}</string>", plist_escape(arg)))
        .collect::<String>();
    let env_xml = if environment.is_empty() {
        String::new()
    } else {
        let items = environment
            .iter()
            .filter(|(_, value)| !value.trim().is_empty())
            .map(|(key, value)| {
                format!(
                    "\n      <key>{}</key>\n      <string>{}</string>",
                    plist_escape(key),
                    plist_escape(value)
                )
            })
            .collect::<String>();
        format!("\n    <key>EnvironmentVariables</key>\n    <dict>{items}\n    </dict>")
    };

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n  <dict>\n    <key>Label</key>\n    <string>{}</string>\n    <key>Comment</key>\n    <string>{}</string>\n    <key>RunAtLoad</key>\n    <true/>\n    <key>KeepAlive</key>\n    <true/>\n    <key>ThrottleInterval</key>\n    <integer>{}</integer>\n    <key>Umask</key>\n    <integer>{}</integer>\n    <key>ProgramArguments</key>\n    <array>{}\n    </array>\n    <key>WorkingDirectory</key>\n    <string>{}</string>\n    <key>StandardOutPath</key>\n    <string>{}</string>\n    <key>StandardErrorPath</key>\n    <string>{}</string>{}\n  </dict>\n</plist>\n",
        plist_escape(label),
        plist_escape(comment),
        LAUNCH_AGENT_THROTTLE_INTERVAL_SECONDS,
        LAUNCH_AGENT_UMASK_DECIMAL,
        args_xml,
        plist_escape(&display_path(working_directory)),
        plist_escape(&display_path(stdout_path)),
        plist_escape(&display_path(stderr_path)),
        env_xml,
    )
}

fn build_systemd_unit(
    description: &str,
    program_arguments: &[String],
    working_directory: &Path,
    stdout_path: &Path,
    stderr_path: &Path,
    environment: &BTreeMap<String, String>,
    output_mode: SystemdOutputMode,
) -> Result<String, String> {
    validate_systemd_value("Description", description)?;
    validate_systemd_value("WorkingDirectory", &display_path(working_directory))?;
    validate_systemd_value("StandardOutput", &display_path(stdout_path))?;
    validate_systemd_value("StandardError", &display_path(stderr_path))?;
    for argument in program_arguments {
        validate_systemd_value("ExecStart", argument)?;
    }
    for (key, value) in environment {
        validate_systemd_environment_key(key)?;
        validate_systemd_value("Environment value", value)?;
    }
    let exec_start = program_arguments
        .iter()
        .map(|arg| systemd_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let environment_lines = environment
        .iter()
        .filter(|(_, value)| !value.trim().is_empty())
        .map(|(key, value)| {
            format!(
                "Environment=\"{}={}\"",
                systemd_escape(key),
                systemd_escape(value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let environment_block = if environment_lines.is_empty() {
        String::new()
    } else {
        format!("{environment_lines}\n")
    };

    Ok(format!(
        "[Unit]\nDescription={}\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory={}\nExecStart={}\n{}StandardOutput={}\nStandardError={}\nRestart=always\nRestartSec=1\nUMask=0077\n\n[Install]\nWantedBy=default.target\n",
        systemd_escape(description),
        systemd_quote(&display_path(working_directory)),
        exec_start,
        environment_block,
        systemd_quote(&systemd_output_target(stdout_path, output_mode)),
        systemd_quote(&systemd_output_target(stderr_path, output_mode)),
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SystemdOutputMode {
    Journal,
    Append,
}

fn systemd_output_mode(env: &BTreeMap<String, String>) -> SystemdOutputMode {
    let systemctl = env
        .get(SYSTEMCTL_BIN_OVERRIDE)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("systemctl");
    let version = Command::new(systemctl)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| systemd_version(&output.stdout));
    systemd_output_mode_for_version(version)
}

fn systemd_version(stdout: &[u8]) -> Option<u32> {
    String::from_utf8_lossy(stdout)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|version| version.parse::<u32>().ok())
}

fn systemd_output_mode_for_version(version: Option<u32>) -> SystemdOutputMode {
    match version {
        Some(240..) => SystemdOutputMode::Append,
        _ => SystemdOutputMode::Journal,
    }
}

fn systemd_output_target(path: &Path, mode: SystemdOutputMode) -> String {
    match mode {
        SystemdOutputMode::Journal => "journal".to_string(),
        SystemdOutputMode::Append => format!("append:{}", display_path(path)),
    }
}

fn plist_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    set_mode(path, SERVICE_DIR_MODE)
}

pub(crate) fn ensure_service_definition_dir(path: &Path) -> Result<(), String> {
    let existed = path.exists();
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    if existed {
        #[cfg(unix)]
        {
            let mode = fs::metadata(path)
                .map_err(|error| error.to_string())?
                .permissions()
                .mode();
            let writable_by_others = mode & 0o022 != 0;
            let sticky = mode & 0o1000 != 0;
            if writable_by_others && !sticky {
                return Err(format!(
                    "service definition directory {} is group/world-writable; remove those write permissions before installing the service",
                    display_path(path)
                ));
            }
        }
        return Ok(());
    }
    set_mode(path, SERVICE_DIR_MODE)
}

fn write_private_service_file(path: &Path, raw: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("service definition has no parent: {}", display_path(path)))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            format!(
                "service definition has no file name: {}",
                display_path(path)
            )
        })?;
    let temp_path = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(SERVICE_FILE_MODE);
    let mut file = options
        .open(&temp_path)
        .map_err(|error| error.to_string())?;
    let result = (|| {
        file.write_all(raw).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        set_mode(&temp_path, SERVICE_FILE_MODE)?;
        fs::rename(&temp_path, path).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions).map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

fn activate_launchd_service(
    label: &str,
    definition_path: &Path,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let domain = gui_domain(env)?;
    let definition_path = display_path(definition_path);
    let target = format!("{domain}/{label}");
    let _ = run_launchctl(env, ["bootout", target.as_str()]);
    let _ = run_launchctl(env, ["bootout", domain.as_str(), definition_path.as_str()]);
    let _ = run_launchctl(env, ["unload", definition_path.as_str()]);
    let _ = run_launchctl(env, ["enable", target.as_str()]);
    let bootstrap = run_launchctl(
        env,
        ["bootstrap", domain.as_str(), definition_path.as_str()],
    )?;
    if !bootstrap.status.success() {
        return Err(format!(
            "launchctl bootstrap failed: {}",
            launchctl_detail(&bootstrap)
        ));
    }
    Ok(())
}

fn activate_systemd_user_service(
    label: &str,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    register_systemd_user_service(label, env)?;

    let restart = run_systemctl(env, ["--user", "restart", label])?;
    if restart.status.success() {
        return Ok(());
    }

    let start = run_systemctl(env, ["--user", "start", label])?;
    if !start.status.success() {
        return Err(format!(
            "systemctl --user restart/start failed: {}; {}",
            systemctl_detail(&restart),
            systemctl_detail(&start)
        ));
    }

    Ok(())
}

fn register_systemd_user_service(
    label: &str,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let reload = run_systemctl(env, ["--user", "daemon-reload"])?;
    if !reload.status.success() {
        return Err(format!(
            "systemctl --user daemon-reload failed: {}",
            systemctl_detail(&reload)
        ));
    }

    let enable = run_systemctl(env, ["--user", "enable", label])?;
    if !enable.status.success() {
        return Err(format!(
            "systemctl --user enable failed: {}",
            systemctl_detail(&enable)
        ));
    }
    Ok(())
}

fn restore_systemd_user_service_registration(
    label: &str,
    running: bool,
    enablement: Option<ManagedServiceEnablement>,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let previous_enablement =
        enablement.ok_or_else(|| format!("failed to capture the enabled state for {label}"))?;
    let reload = run_systemctl(env, ["--user", "daemon-reload"])?;
    if !reload.status.success() {
        return Err(format!(
            "systemctl --user daemon-reload failed: {}",
            systemctl_detail(&reload)
        ));
    }
    let running_result = if running {
        let restart = run_systemctl(env, ["--user", "restart", label])?;
        if restart.status.success() {
            Ok(())
        } else {
            let start = run_systemctl(env, ["--user", "start", label])?;
            if start.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "systemctl --user restart/start failed while restoring {label}: {}; {}",
                    systemctl_detail(&restart),
                    systemctl_detail(&start)
                ))
            }
        }
    } else {
        Ok(())
    };
    let enablement_result = set_systemd_service_enablement(label, previous_enablement, env);
    match (running_result, enablement_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(enablement_error)) => Err(format!(
            "{error}; failed to restore the previous systemd enablement: {enablement_error}"
        )),
    }
}

fn set_systemd_service_enablement(
    label: &str,
    enablement: ManagedServiceEnablement,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let (action, output) = match enablement {
        ManagedServiceEnablement::Enabled => {
            ("enable", run_systemctl(env, ["--user", "enable", label])?)
        }
        ManagedServiceEnablement::EnabledRuntime => (
            "enable --runtime",
            run_systemctl(env, ["--user", "enable", "--runtime", label])?,
        ),
        ManagedServiceEnablement::Disabled => {
            ("disable", run_systemctl(env, ["--user", "disable", label])?)
        }
    };
    if !output.status.success() {
        return Err(format!(
            "systemctl --user {action} failed while restoring {label}: {}",
            systemctl_detail(&output)
        ));
    }
    Ok(())
}

fn gui_domain(env: &BTreeMap<String, String>) -> Result<String, String> {
    let id_bin = env
        .get(ID_BIN_OVERRIDE)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("/usr/bin/id");
    Command::new(id_bin)
        .arg("-u")
        .output()
        .map_err(|error| {
            format!("failed to determine the current UID with \"{id_bin} -u\": {error}")
        })
        .and_then(|output| {
            if !output.status.success() {
                return Err(format!(
                    "\"{id_bin} -u\" failed while determining the launchd domain"
                ));
            }
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<u32>()
                .map(|uid| format!("gui/{uid}"))
                .map_err(|_| format!("\"{id_bin} -u\" returned an invalid UID"))
        })
}

fn run_launchctl<const N: usize>(
    env: &BTreeMap<String, String>,
    args: [&str; N],
) -> Result<Output, String> {
    run_service_manager_command(
        service_manager_binary(env, ServiceManagerKind::Launchd),
        args,
    )
    .map_err(|error| format!("failed to run \"launchctl\": {error}"))
}

fn run_systemctl<const N: usize>(
    env: &BTreeMap<String, String>,
    args: [&str; N],
) -> Result<Output, String> {
    run_service_manager_command(
        service_manager_binary(env, ServiceManagerKind::SystemdUser),
        args,
    )
    .map_err(|error| format!("failed to run \"systemctl\": {error}"))
}

fn run_service_manager_command<const N: usize>(
    binary: &str,
    args: [&str; N],
) -> std::io::Result<Output> {
    retry_service_manager_executable_busy(|| Command::new(binary).args(args).output())
}

fn retry_service_manager_executable_busy<T>(
    mut operation: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    for attempt in 0..SERVICE_MANAGER_BUSY_ATTEMPTS {
        match operation() {
            Ok(output) => return Ok(output),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt + 1 < SERVICE_MANAGER_BUSY_ATTEMPTS =>
            {
                sleep(Duration::from_millis(SERVICE_MANAGER_BUSY_RETRY_MS));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("busy retry loop always returns on its final attempt")
}

fn launchctl_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn systemctl_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn systemd_quote(value: &str) -> String {
    if value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\\'))
    {
        format!("\"{}\"", systemd_escape(value))
    } else {
        systemd_escape(value)
    }
}

fn systemd_escape(value: &str) -> String {
    systemd_legacy_escape(value).replace('%', "%%")
}

fn systemd_legacy_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn validate_systemd_value(label: &str, value: &str) -> Result<(), String> {
    if value.contains(['\r', '\n']) {
        return Err(format!("{label} cannot contain a line break"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label} cannot contain control characters"));
    }
    Ok(())
}

fn validate_systemd_environment_key(key: &str) -> Result<(), String> {
    validate_systemd_value("Environment key", key)?;
    if key.is_empty() || key.contains('=') {
        return Err("Environment key must be non-empty and cannot contain '='".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::store::display_path;

    use super::{
        ManagedServiceDefinition, ManagedServiceEnablement, ManagedServiceIdentity,
        OCM_SERVICE_LABEL, SERVICE_MANAGER_BUSY_ATTEMPTS, ServiceManagerKind, gui_domain,
        managed_service_identity, managed_service_label, restore_managed_service_registration,
        retry_service_manager_executable_busy, service_backend_support_error,
        service_definition_dir, service_manager_kind, systemd_output_mode_for_version,
        write_managed_service_definition,
    };

    #[test]
    fn service_manager_execution_retries_a_busy_executable() {
        let attempts = Cell::new(0);

        let result = retry_service_manager_executable_busy(|| {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            if attempt == 0 {
                Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
            } else {
                Ok(())
            }
        });

        assert!(result.is_ok());
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn service_manager_execution_does_not_retry_other_errors() {
        let attempts = Cell::new(0);

        let result: std::io::Result<()> = retry_service_manager_executable_busy(|| {
            attempts.set(attempts.get() + 1);
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });

        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn service_manager_execution_stops_at_the_busy_retry_limit() {
        let attempts = Cell::new(0);

        let result: std::io::Result<()> = retry_service_manager_executable_busy(|| {
            attempts.set(attempts.get() + 1);
            Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
        });

        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::ExecutableFileBusy
        );
        assert_eq!(attempts.get(), SERVICE_MANAGER_BUSY_ATTEMPTS);
    }

    #[test]
    fn manager_override_supports_systemd_user() {
        let mut env = BTreeMap::new();
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "systemd-user".to_string(),
        );
        assert_eq!(service_manager_kind(&env), ServiceManagerKind::SystemdUser);
    }

    #[test]
    fn systemd_output_modes_follow_supported_versions() {
        assert_eq!(
            systemd_output_mode_for_version(Some(235)),
            super::SystemdOutputMode::Journal
        );
        assert_eq!(
            systemd_output_mode_for_version(Some(236)),
            super::SystemdOutputMode::Journal
        );
        assert_eq!(
            systemd_output_mode_for_version(Some(239)),
            super::SystemdOutputMode::Journal
        );
        assert_eq!(
            systemd_output_mode_for_version(Some(240)),
            super::SystemdOutputMode::Append
        );
        assert_eq!(
            systemd_output_mode_for_version(None),
            super::SystemdOutputMode::Journal
        );
    }

    #[cfg(unix)]
    #[test]
    fn registering_systemd_service_does_not_start_it() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ocm-platform-register-systemd-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let systemctl = root.join("systemctl");
        let log = root.join("systemctl.log");
        fs::write(
            &systemctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
                display_path(&log)
            ),
        )
        .unwrap();
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755)).unwrap();
        let env = BTreeMap::from([
            (
                "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
                "systemd-user".to_string(),
            ),
            (
                "OCM_INTERNAL_SYSTEMCTL_BIN".to_string(),
                display_path(&systemctl),
            ),
        ]);

        restore_managed_service_registration(
            "ai.openclaw.ocm",
            Path::new("/unused"),
            false,
            Some(ManagedServiceEnablement::Enabled),
            &env,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            "--user daemon-reload\n--user enable ai.openclaw.ocm\n"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn restoring_stopped_launchd_service_keeps_it_loaded_without_running() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ocm-platform-restore-launchd-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let launchctl = root.join("launchctl");
        let log = root.join("launchctl.log");
        let state = root.join("launchctl-state.txt");
        let definition = root.join("ai.openclaw.ocm.plist");
        fs::write(
            &launchctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in\n  bootout|unload) rm -f '{}';;\n  bootstrap) printf 'path = {}\\nstate = running\\npid = 42\\n' > '{}';;\n  kill) printf 'path = {}\\nstate = waiting\\n' > '{}';;\n  print) if [ -f '{}' ]; then cat '{}'; else printf 'Could not find service\\n' >&2; exit 1; fi;;\nesac\n",
                display_path(&log),
                display_path(&state),
                display_path(&definition),
                display_path(&state),
                display_path(&definition),
                display_path(&state),
                display_path(&state),
                display_path(&state),
            ),
        )
        .unwrap();
        fs::set_permissions(&launchctl, fs::Permissions::from_mode(0o755)).unwrap();
        let env = BTreeMap::from([
            (
                "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
                "launchd".to_string(),
            ),
            (
                "OCM_INTERNAL_LAUNCHCTL_BIN".to_string(),
                display_path(&launchctl),
            ),
        ]);

        restore_managed_service_registration(
            "ai.openclaw.ocm",
            &definition,
            false,
            Some(ManagedServiceEnablement::Enabled),
            &env,
        )
        .unwrap();

        assert!(
            fs::read_to_string(&state)
                .unwrap()
                .contains("state = waiting")
        );
        let commands = fs::read_to_string(&log).unwrap();
        assert!(
            commands
                .lines()
                .any(|line| line.starts_with("disable gui/"))
        );
        assert!(
            commands
                .lines()
                .any(|line| line.starts_with("kill SIGTERM gui/"))
        );
        assert!(
            commands
                .lines()
                .last()
                .is_some_and(|line| line.starts_with("enable gui/"))
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn manager_override_supports_unsupported_backends() {
        let mut env = BTreeMap::new();
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "unsupported".to_string(),
        );
        assert_eq!(service_manager_kind(&env), ServiceManagerKind::Unsupported);
    }

    #[test]
    fn backend_support_error_reports_missing_launchctl_binary() {
        let mut env = BTreeMap::new();
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "launchd".to_string(),
        );
        env.insert(
            "OCM_INTERNAL_LAUNCHCTL_BIN".to_string(),
            "/tmp/ocm-tests/missing-launchctl".to_string(),
        );

        assert_eq!(
            service_backend_support_error(&env),
            Some(
                "managed services require launchctl on this machine; run OpenClaw directly inside the env for now"
                    .to_string()
            )
        );
    }

    #[test]
    fn backend_support_error_reports_missing_systemctl_binary() {
        let mut env = BTreeMap::new();
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "systemd-user".to_string(),
        );
        env.insert(
            "OCM_INTERNAL_SYSTEMCTL_BIN".to_string(),
            "/tmp/ocm-tests/missing-systemctl".to_string(),
        );

        assert_eq!(
            service_backend_support_error(&env),
            Some(
                "managed services require systemctl --user on this machine; run OpenClaw directly inside the env for now"
                    .to_string()
            )
        );
    }

    #[test]
    fn backend_support_error_reports_unusable_launchctl_session() {
        let mut env = BTreeMap::new();
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "launchd".to_string(),
        );
        env.insert(
            "OCM_INTERNAL_LAUNCHCTL_BIN".to_string(),
            "/bin/sh".to_string(),
        );
        env.insert("HOME".to_string(), "/tmp".to_string());

        assert_eq!(
            service_backend_support_error(&env),
            Some(
                "managed services require a usable launchctl session on this machine; run OpenClaw directly inside the env for now"
                    .to_string()
            )
        );
    }

    #[test]
    fn backend_support_error_reports_unusable_systemd_session() {
        let mut env = BTreeMap::new();
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "systemd-user".to_string(),
        );
        env.insert(
            "OCM_INTERNAL_SYSTEMCTL_BIN".to_string(),
            "/bin/sh".to_string(),
        );
        env.insert("HOME".to_string(), "/tmp".to_string());

        assert_eq!(
            service_backend_support_error(&env),
            Some(
                "managed services require a usable systemctl --user session on this machine; run OpenClaw directly inside the env for now"
                    .to_string()
            )
        );
    }

    #[test]
    fn launchd_domain_never_guesses_a_uid() {
        let env = BTreeMap::from([(
            "OCM_INTERNAL_ID_BIN".to_string(),
            "/definitely/missing/id".to_string(),
        )]);
        let error = gui_domain(&env).unwrap_err();
        assert!(error.contains("failed to determine the current UID"));
    }

    #[test]
    fn service_paths_follow_the_selected_backend() {
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), "/tmp/home".to_string());
        env.insert("OCM_HOME".to_string(), "/tmp/store".to_string());
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "systemd-user".to_string(),
        );

        assert_eq!(
            service_definition_dir(&env).display().to_string(),
            "/tmp/home/.config/systemd/user"
        );
        assert_eq!(
            managed_service_identity(&env, Path::new("/tmp"))
                .unwrap()
                .definition_path
                .display()
                .to_string(),
            format!(
                "/tmp/home/.config/systemd/user/{}.service",
                managed_service_label()
            )
        );
    }

    #[test]
    fn managed_service_label_is_stable() {
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), "/tmp/home".to_string());
        env.insert("OCM_HOME".to_string(), "/tmp/store".to_string());

        let label = managed_service_label();
        assert_eq!(label, OCM_SERVICE_LABEL);
        assert!(matches!(
            managed_service_identity(&env, Path::new("/tmp")).unwrap(),
            ManagedServiceIdentity {
                label: managed_label,
                definition_path,
                ..
            } if managed_label == label && definition_path.display().to_string().contains(label)
        ));
    }

    #[test]
    fn launch_agent_definition_includes_program_arguments_and_environment() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ocm-platform-launchd-{unique}"));
        fs::create_dir_all(root.join("home")).unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), display_path(&root.join("home")));
        env.insert("OCM_HOME".to_string(), display_path(&root.join("store")));
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "launchd".to_string(),
        );

        let definition = ManagedServiceDefinition {
            label: OCM_SERVICE_LABEL.to_string(),
            description: "test".to_string(),
            definition_path: root.join("home/Library/LaunchAgents/test.plist"),
            program_arguments: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "openclaw gateway run".to_string(),
            ],
            working_directory: Path::new("/tmp/work").to_path_buf(),
            stdout_path: root.join("logs/stdout.log"),
            stderr_path: root.join("logs/stderr.log"),
            environment: BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
        };

        write_managed_service_definition(&definition, &env).unwrap();
        let plist = fs::read_to_string(&definition.definition_path).unwrap();
        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains(OCM_SERVICE_LABEL));
        assert!(plist.contains("openclaw gateway run"));
        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(plist.contains(&display_path(&definition.stdout_path)));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn systemd_definition_includes_exec_start_and_environment() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ocm-platform-systemd-{unique}"));
        fs::create_dir_all(root.join("home")).unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), display_path(&root.join("home")));
        env.insert("OCM_HOME".to_string(), display_path(&root.join("store")));
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "systemd-user".to_string(),
        );
        env.insert(
            "OCM_INTERNAL_SYSTEMCTL_BIN".to_string(),
            "/definitely/missing/systemctl".to_string(),
        );

        let definition = ManagedServiceDefinition {
            label: OCM_SERVICE_LABEL.to_string(),
            description: "test".to_string(),
            definition_path: root.join("home/.config/systemd/user/test.service"),
            program_arguments: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "openclaw gateway run --port 18789".to_string(),
            ],
            working_directory: Path::new("/tmp/work").to_path_buf(),
            stdout_path: root.join("logs/stdout.log"),
            stderr_path: root.join("logs/stderr.log"),
            environment: BTreeMap::from([
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("OPENCLAW_HOME".to_string(), "/tmp/demo".to_string()),
            ]),
        };

        write_managed_service_definition(&definition, &env).unwrap();
        let unit = fs::read_to_string(&definition.definition_path).unwrap();
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("ExecStart=/bin/sh -lc"));
        assert!(unit.contains("WorkingDirectory=/tmp/work"));
        assert!(unit.contains("Environment=\"OPENCLAW_HOME=/tmp/demo\""));
        assert!(unit.contains("StandardOutput=journal"));
        assert!(unit.contains("StandardError=journal"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn service_definition_and_logs_are_private_without_widening_existing_parent() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ocm-platform-private-{unique}"));
        let definition_parent = root.join("home/.config/systemd/user");
        fs::create_dir_all(&definition_parent).unwrap();
        fs::set_permissions(&definition_parent, fs::Permissions::from_mode(0o750)).unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), display_path(&root.join("home")));
        env.insert("OCM_HOME".to_string(), display_path(&root.join("store")));
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "systemd-user".to_string(),
        );
        let definition = ManagedServiceDefinition {
            label: OCM_SERVICE_LABEL.to_string(),
            description: "test".to_string(),
            definition_path: definition_parent.join("test.service"),
            program_arguments: vec!["/bin/true".to_string()],
            working_directory: root.join("store"),
            stdout_path: root.join("logs/stdout.log"),
            stderr_path: root.join("logs/stderr.log"),
            environment: BTreeMap::new(),
        };

        write_managed_service_definition(&definition, &env).unwrap();

        assert_eq!(
            fs::metadata(&definition_parent)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            fs::metadata(&definition.definition_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(root.join("logs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn service_definition_rejects_a_writable_existing_parent() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ocm-platform-writable-parent-{unique}"));
        let definition_parent = root.join("home/.config/systemd/user");
        fs::create_dir_all(&definition_parent).unwrap();
        fs::set_permissions(&definition_parent, fs::Permissions::from_mode(0o775)).unwrap();
        let env = BTreeMap::from([
            ("HOME".to_string(), display_path(&root.join("home"))),
            ("OCM_HOME".to_string(), display_path(&root.join("store"))),
            (
                "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
                "systemd-user".to_string(),
            ),
        ]);
        let definition = ManagedServiceDefinition {
            label: OCM_SERVICE_LABEL.to_string(),
            description: "test".to_string(),
            definition_path: definition_parent.join("test.service"),
            program_arguments: vec!["/bin/true".to_string()],
            working_directory: root.join("store"),
            stdout_path: root.join("logs/stdout.log"),
            stderr_path: root.join("logs/stderr.log"),
            environment: BTreeMap::new(),
        };

        let error = write_managed_service_definition(&definition, &env).unwrap_err();
        assert!(error.contains("group/world-writable"));
        assert!(!definition.definition_path.exists());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn systemd_definition_escapes_specifiers_and_rejects_line_breaks() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ocm-platform-systemd-escape-{unique}"));
        fs::create_dir_all(root.join("home")).unwrap();
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), display_path(&root.join("home")));
        env.insert("OCM_HOME".to_string(), display_path(&root.join("store")));
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "systemd-user".to_string(),
        );
        let mut definition = ManagedServiceDefinition {
            label: OCM_SERVICE_LABEL.to_string(),
            description: "OCM %p".to_string(),
            definition_path: root.join("home/.config/systemd/user/test.service"),
            program_arguments: vec!["/tmp/%p/ocm".to_string()],
            working_directory: root.join("store-%p"),
            stdout_path: root.join("logs/%p.stdout.log"),
            stderr_path: root.join("logs/%p.stderr.log"),
            environment: BTreeMap::from([("VALUE".to_string(), "%p".to_string())]),
        };

        write_managed_service_definition(&definition, &env).unwrap();
        let unit = fs::read_to_string(&definition.definition_path).unwrap();
        assert!(unit.contains("Description=OCM %%p"), "{unit}");
        assert!(unit.contains("/tmp/%%p/ocm"), "{unit}");
        assert!(unit.contains("VALUE=%%p"), "{unit}");

        definition.description = "bad\nRestart=no".to_string();
        let error = write_managed_service_definition(&definition, &env).unwrap_err();
        assert!(error.contains("Description cannot contain a line break"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stable_service_identity_rejects_a_different_store_owner() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ocm-platform-owner-{unique}"));
        fs::create_dir_all(root.join("home")).unwrap();
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), display_path(&root.join("home")));
        env.insert("OCM_HOME".to_string(), display_path(&root.join("store-a")));
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "systemd-user".to_string(),
        );
        let definition_path = root.join("home/.config/systemd/user/ai.openclaw.ocm.service");
        let definition = ManagedServiceDefinition {
            label: OCM_SERVICE_LABEL.to_string(),
            description: "store a".to_string(),
            definition_path: definition_path.clone(),
            program_arguments: vec!["/bin/true".to_string()],
            working_directory: root.join("store-a"),
            stdout_path: root.join("store-a/logs/stdout.log"),
            stderr_path: root.join("store-a/logs/stderr.log"),
            environment: BTreeMap::from([(
                "OCM_HOME".to_string(),
                display_path(&root.join("store-a")),
            )]),
        };
        write_managed_service_definition(&definition, &env).unwrap();
        let original = fs::read_to_string(&definition_path).unwrap();

        let mut other = definition;
        other.description = "store b".to_string();
        other.working_directory = root.join("store-b");
        other
            .environment
            .insert("OCM_HOME".to_string(), display_path(&root.join("store-b")));
        let error = write_managed_service_definition(&other, &env).unwrap_err();
        assert!(error.contains("already bound to a different OCM_HOME"));
        assert_eq!(fs::read_to_string(&definition_path).unwrap(), original);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stable_service_identity_accepts_legacy_systemd_percent_escaping() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ocm-platform-owner-percent-{unique}"));
        fs::create_dir_all(root.join("home")).unwrap();
        let store = root.join("store%1");
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), display_path(&root.join("home")));
        env.insert("OCM_HOME".to_string(), display_path(&store));
        env.insert(
            "OCM_INTERNAL_SERVICE_MANAGER".to_string(),
            "systemd-user".to_string(),
        );
        let definition = ManagedServiceDefinition {
            label: OCM_SERVICE_LABEL.to_string(),
            description: "percent store".to_string(),
            definition_path: root.join("home/.config/systemd/user/ai.openclaw.ocm.service"),
            program_arguments: vec!["/bin/true".to_string()],
            working_directory: store.clone(),
            stdout_path: store.join("logs/stdout.log"),
            stderr_path: store.join("logs/stderr.log"),
            environment: BTreeMap::from([("OCM_HOME".to_string(), display_path(&store))]),
        };

        write_managed_service_definition(&definition, &env).unwrap();
        let current = fs::read_to_string(&definition.definition_path).unwrap();
        let legacy = current.replace("store%%1", "store%1");
        fs::write(&definition.definition_path, legacy).unwrap();

        write_managed_service_definition(&definition, &env).unwrap();
        let rewritten = fs::read_to_string(&definition.definition_path).unwrap();
        assert!(rewritten.contains("store%%1"));

        fs::remove_dir_all(&root).unwrap();
    }
}
