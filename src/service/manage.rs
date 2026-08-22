use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use serde::Serialize;

use super::inspect::ServiceSummary;
use super::readiness::GatewayReadiness;
use super::service_backend_support_error;
use crate::env::{EnvironmentService, ResolvedExecution};
use crate::infra::shell::{build_openclaw_dev_source_env, build_openclaw_env};
use crate::managed_node::apply_path_prepend_to_environment;
use crate::store::{restore_environment_service_policy, set_environment_service_policy};
use crate::supervisor::{SupervisorService, sync_supervisor_env_if_present};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceActionSummary {
    pub env_name: String,
    pub service_kind: String,
    pub action: String,
    pub installed: bool,
    pub loaded: bool,
    pub running: bool,
    pub desired_running: bool,
    pub gateway_port: u32,
    pub gateway_state: String,
    pub gateway_ready: Option<bool>,
    pub issue: Option<String>,
    pub binding_kind: Option<String>,
    pub binding_name: Option<String>,
    pub stdout_path: Option<String>,
    pub stderr_path: Option<String>,
    pub warnings: Vec<String>,
}

pub type ServiceInstallSummary = ServiceActionSummary;

impl ServiceActionSummary {
    pub(crate) fn apply_gateway_readiness(&mut self, readiness: GatewayReadiness) {
        self.loaded = readiness.status.loaded;
        self.running = readiness.status.running;
        self.desired_running = readiness.status.desired_running;
        self.gateway_port = readiness.status.gateway_port;
        self.gateway_state = readiness.status.gateway_state;
        self.gateway_ready = Some(readiness.ready);
        self.issue = readiness.issue;
        self.stdout_path = readiness.status.stdout_path;
        self.stderr_path = readiness.status.stderr_path;
    }

    pub(crate) fn ensure_gateway_ready(&self) -> Result<(), String> {
        if self.gateway_ready == Some(false) {
            Err(self
                .issue
                .clone()
                .unwrap_or_else(|| "gateway did not become ready".to_string()))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ServiceRestartOptions {
    pub force: bool,
}

pub(crate) fn restart_originates_inside_gateway(
    name: &str,
    env: &BTreeMap<String, String>,
) -> bool {
    env.get("OCM_ACTIVE_ENV").map(String::as_str) == Some(name)
        && env.get("OPENCLAW_SERVICE_KIND").map(String::as_str) == Some("gateway")
}

#[derive(Clone, Copy)]
enum ServiceSupervisorPolicy {
    LeaveAsIs,
    EnsureRunning,
}

#[derive(Clone, Copy)]
enum ServiceUpdate {
    Install,
    Start,
    Stop,
    Restart,
    Uninstall,
}

impl ServiceUpdate {
    fn settings(
        self,
    ) -> (
        &'static str,
        Option<bool>,
        Option<bool>,
        bool,
        ServiceSupervisorPolicy,
    ) {
        match self {
            Self::Install => (
                "install",
                Some(true),
                Some(false),
                true,
                ServiceSupervisorPolicy::EnsureRunning,
            ),
            Self::Start => (
                "start",
                Some(true),
                Some(true),
                true,
                ServiceSupervisorPolicy::EnsureRunning,
            ),
            Self::Stop => (
                "stop",
                Some(true),
                Some(false),
                false,
                ServiceSupervisorPolicy::LeaveAsIs,
            ),
            Self::Restart => (
                "restart",
                Some(true),
                Some(true),
                false,
                ServiceSupervisorPolicy::EnsureRunning,
            ),
            Self::Uninstall => (
                "uninstall",
                Some(false),
                Some(false),
                false,
                ServiceSupervisorPolicy::LeaveAsIs,
            ),
        }
    }
}

struct RestartActionStatus {
    summary: ServiceSummary,
    warnings: Vec<String>,
    observed_restart: bool,
}

pub fn install_service(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ServiceInstallSummary, String> {
    update_service(name, ServiceUpdate::Install, env, cwd)
}

pub fn start_service(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ServiceActionSummary, String> {
    update_service(name, ServiceUpdate::Start, env, cwd)
}

pub fn stop_service(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ServiceActionSummary, String> {
    update_service(name, ServiceUpdate::Stop, env, cwd)
}

pub fn restart_service(
    name: &str,
    options: ServiceRestartOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ServiceActionSummary, String> {
    ensure_gateway_binding(name, env, cwd)?;
    let env_service = EnvironmentService::new(env, cwd);
    let meta = env_service.get(name)?;
    if !(meta.service_enabled && meta.service_running) {
        return update_service(name, ServiceUpdate::Restart, env, cwd);
    }

    let before = super::inspect::service_status_fast(name, env, cwd)?;
    if !before.ocm_service_running {
        return update_service(name, ServiceUpdate::Restart, env, cwd);
    }

    if options.force {
        return restart_running_service_directly(
            name,
            before,
            env,
            cwd,
            "forced supervisor restart bypassed OpenClaw restart recovery handoff",
        );
    }

    if before.restart_handoff.as_deref() != Some("protocol-v1") {
        return restart_running_service_directly(
            name,
            before,
            env,
            cwd,
            "OpenClaw restart recovery handoff protocol v1 is unavailable; used the legacy direct supervisor restart path, so in-flight work may have been interrupted",
        );
    }

    spawn_recovery_aware_restart(name, env, cwd)?;

    if restart_originates_inside_gateway(name, env) {
        let mut warnings = vec![
            "recovery-aware restart was scheduled without waiting because the request originated inside the target gateway; OpenClaw will preserve and resume eligible interrupted work after restart"
                .to_string(),
        ];
        let summary = super::inspect::service_status_fast(name, env, cwd)?;
        if !summary.running {
            warnings
                .push("gateway is already transitioning to its replacement process".to_string());
        }
        return Ok(service_action_summary("restart", summary, warnings));
    }

    let status = wait_for_restart_action_summary(name, before.child_pid, env, cwd)?;
    let mut warnings = status.warnings;
    if !status.observed_restart {
        warnings.push(
            "recovery-aware restart was accepted, but OCM did not observe a replacement gateway within 30 seconds; no direct supervisor restart was attempted"
                .to_string(),
        );
    }
    Ok(service_action_summary("restart", status.summary, warnings))
}

fn restart_running_service_directly(
    name: &str,
    before: ServiceSummary,
    env: &BTreeMap<String, String>,
    cwd: &Path,
    restart_warning: &str,
) -> Result<ServiceActionSummary, String> {
    let supervisor = SupervisorService::new(env, cwd);
    let mut request_id = supervisor.request_child_restart(name)?;
    let restart_result = wait_for_restart_action_summary(name, before.child_pid, env, cwd);
    match restart_result {
        Ok(mut status) => {
            if !status.observed_restart {
                let (_, recovery_request_id) = supervisor
                    .recover_child_restart_with_request_id(name)
                    .map_err(|error| {
                        format!(
                            "gateway restart was not observed and targeted supervisor recovery failed: {error}"
                        )
                    })?;
                request_id = recovery_request_id;
                status = wait_for_restart_action_summary_with_timeout(
                    name,
                    before.child_pid,
                    Duration::from_secs(5),
                    env,
                    cwd,
                )?;
                if !status.observed_restart {
                    return Err(
                        "gateway restart was not observed after targeted supervisor recovery"
                            .to_string(),
                    );
                }
                status
                    .warnings
                    .push("gateway restart required targeted supervisor recovery".to_string());
            }
            if let Err(clear_error) = supervisor.clear_child_restart_request(name, &request_id) {
                status.warnings.push(format!(
                    "restart completed, but failed to clear restart request: {clear_error}"
                ));
            }
            status.warnings.push(restart_warning.to_string());
            Ok(service_action_summary(
                "restart",
                status.summary,
                status.warnings,
            ))
        }
        Err(restart_error) => Err(restart_error),
    }
}

fn spawn_recovery_aware_restart(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    let args = vec![
        "gateway".to_string(),
        "restart".to_string(),
        "--force".to_string(),
        "--json".to_string(),
    ];
    let resolved = EnvironmentService::new(env, cwd).resolve(name, None, None, &args)?;
    let command = resolved_restart_command(resolved, env)?;
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .envs(&command.env)
        .current_dir(&command.cwd);
    #[cfg(unix)]
    process.process_group(0);
    let mut child = process
        .spawn()
        .map_err(|error| format!("failed to run \"{}\": {error}", command.program))?;

    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "OpenClaw rejected the recovery-aware restart for env \"{name}\" (exit code {}); no direct supervisor restart was attempted. Inspect the gateway logs or use \"ocm service restart {name} --force\" to bypass OpenClaw recovery handoff",
                    status.code().unwrap_or(1)
                ));
            }
            Ok(None) => sleep(Duration::from_millis(25)),
            Err(error) => {
                return Err(format!(
                    "failed to inspect the recovery-aware restart helper for env \"{name}\": {error}"
                ));
            }
        }
    }
    Ok(())
}

struct ResolvedRestartCommand {
    program: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
}

fn resolved_restart_command(
    resolved: ResolvedExecution,
    process_env: &BTreeMap<String, String>,
) -> Result<ResolvedRestartCommand, String> {
    match resolved {
        ResolvedExecution::Launcher {
            env,
            command,
            run_dir,
            ..
        } => {
            let openclaw_env = build_openclaw_env(&env, process_env);
            if cfg!(windows) {
                Ok(ResolvedRestartCommand {
                    program: "cmd".to_string(),
                    args: vec!["/C".to_string(), command],
                    env: openclaw_env,
                    cwd: run_dir,
                })
            } else {
                Ok(ResolvedRestartCommand {
                    program: "sh".to_string(),
                    args: vec!["-lc".to_string(), command],
                    env: openclaw_env,
                    cwd: run_dir,
                })
            }
        }
        ResolvedExecution::Runtime {
            env,
            program,
            program_args,
            path_prepend,
            run_dir,
            ..
        } => {
            let mut openclaw_env = build_openclaw_env(&env, process_env);
            apply_path_prepend_to_environment(&mut openclaw_env, path_prepend.as_deref())?;
            Ok(ResolvedRestartCommand {
                program,
                args: program_args,
                env: openclaw_env,
                cwd: run_dir,
            })
        }
        ResolvedExecution::Dev {
            env,
            program,
            program_args,
            run_dir,
            ..
        }
        | ResolvedExecution::SourceWatch {
            env,
            program,
            program_args,
            run_dir,
            ..
        } => {
            let openclaw_env = build_openclaw_dev_source_env(&env, process_env, &run_dir);
            Ok(ResolvedRestartCommand {
                program,
                args: program_args,
                env: openclaw_env,
                cwd: run_dir,
            })
        }
    }
}

pub fn uninstall_service(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ServiceActionSummary, String> {
    update_service(name, ServiceUpdate::Uninstall, env, cwd)
}

fn ensure_gateway_binding(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    EnvironmentService::new(env, cwd)
        .resolve_gateway_process(name, true)
        .map(|_| ())
}

fn ensure_supervisor_running_locked(
    supervisor: &SupervisorService<'_>,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    if let Some(error) = service_backend_support_error(env) {
        return Err(error);
    }
    supervisor.ensure_daemon_running_locked()?;
    Ok(())
}

fn update_service(
    name: &str,
    update: ServiceUpdate,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ServiceActionSummary, String> {
    let (action, service_enabled, service_running, require_binding, supervisor_policy) =
        update.settings();
    if require_binding {
        ensure_gateway_binding(name, env, cwd)?;
    }
    let supervisor = SupervisorService::new(env, cwd);
    // Service policy and the shared daemon are one lifecycle decision. Holding
    // this lock prevents another store or command from racing ownership/teardown.
    let _lifecycle_lock = supervisor.lock_daemon_lifecycle()?;
    supervisor.validate_daemon_owner_locked()?;
    let daemon_before = supervisor.daemon_status()?;
    let change = set_environment_service_policy(name, service_enabled, service_running, env, cwd)?;
    let update_result = match supervisor_policy {
        ServiceSupervisorPolicy::EnsureRunning => sync_supervisor_env_if_present(env, cwd, name)
            .map(|_| ())
            .and_then(|()| ensure_supervisor_running_locked(&supervisor, env)),
        ServiceSupervisorPolicy::LeaveAsIs => {
            sync_supervisor_env_if_present(env, cwd, name).map(|_| ())
        }
    };
    if let Err(error) = update_result {
        return Err(rollback_service_update(
            error,
            &change,
            &supervisor,
            &daemon_before,
            env,
            cwd,
        ));
    }
    let (mut summary, warnings) = wait_for_action_summary(name, action, env, cwd)?;
    let should_remove_daemon =
        if matches!(update, ServiceUpdate::Stop | ServiceUpdate::Uninstall) && !summary.running {
            match supervisor.has_desired_running_services() {
                Ok(has_desired_running_services) => !has_desired_running_services,
                Err(error) => {
                    return Err(rollback_service_update(
                        error,
                        &change,
                        &supervisor,
                        &daemon_before,
                        env,
                        cwd,
                    ));
                }
            }
        } else {
            false
        };
    if should_remove_daemon {
        if let Err(error) = supervisor.uninstall_daemon_locked() {
            return Err(rollback_service_update(
                error,
                &change,
                &supervisor,
                &daemon_before,
                env,
                cwd,
            ));
        }
        summary = super::inspect::service_status_fast(name, env, cwd)?;
    }
    Ok(service_action_summary(action, summary, warnings))
}

fn rollback_service_update(
    error: String,
    change: &crate::store::EnvironmentServicePolicyChange,
    supervisor: &SupervisorService<'_>,
    daemon_before: &crate::supervisor::SupervisorDaemonSummary,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> String {
    let mut rollback_errors = Vec::new();
    match restore_environment_service_policy(change, env, cwd) {
        Ok(restored) => {
            if restored
                && let Err(rollback_error) =
                    sync_supervisor_env_if_present(env, cwd, &change.applied.name)
            {
                rollback_errors.push(rollback_error);
            }
        }
        Err(rollback_error) => rollback_errors.push(rollback_error),
    }
    if let Err(rollback_error) = supervisor.restore_daemon_state_locked(daemon_before) {
        rollback_errors.push(rollback_error);
    }
    if rollback_errors.is_empty() {
        error
    } else {
        format!(
            "{error}; failed to restore the previous service state: {}",
            rollback_errors.join("; ")
        )
    }
}

fn wait_for_action_summary(
    name: &str,
    action: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(ServiceSummary, Vec<String>), String> {
    let should_wait_for_stop = matches!(action, "stop" | "uninstall");
    if !should_wait_for_stop {
        return Ok((
            super::inspect::service_status_fast(name, env, cwd)?,
            Vec::new(),
        ));
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut latest = super::inspect::service_status_fast(name, env, cwd)?;
    while Instant::now() < deadline {
        if !latest.running {
            return Ok((latest, Vec::new()));
        }
        sleep(Duration::from_millis(100));
        latest = super::inspect::service_status_fast(name, env, cwd)?;
    }

    let mut warnings = Vec::new();
    if latest.running {
        warnings.push(
            "gateway is still shutting down; check service status again in a moment".to_string(),
        );
    }
    Ok((latest, warnings))
}

fn wait_for_restart_action_summary(
    name: &str,
    previous_pid: Option<u32>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RestartActionStatus, String> {
    wait_for_restart_action_summary_with_timeout(
        name,
        previous_pid,
        Duration::from_secs(30),
        env,
        cwd,
    )
}

fn wait_for_restart_action_summary_with_timeout(
    name: &str,
    previous_pid: Option<u32>,
    timeout: Duration,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<RestartActionStatus, String> {
    let deadline = Instant::now() + timeout;
    let mut latest = super::inspect::service_status_fast(name, env, cwd)?;
    while Instant::now() < deadline {
        if latest.running
            && latest
                .child_pid
                .is_some_and(|child_pid| previous_pid != Some(child_pid))
        {
            return Ok(RestartActionStatus {
                summary: latest,
                warnings: Vec::new(),
                observed_restart: true,
            });
        }
        sleep(Duration::from_millis(100));
        latest = super::inspect::service_status_fast(name, env, cwd)?;
    }

    let warning = match previous_pid {
        Some(previous_pid) => {
            format!("gateway restart is still in progress; previous child pid was {previous_pid}")
        }
        None => "gateway restart is still in progress; no replacement child pid has been observed"
            .to_string(),
    };
    Ok(RestartActionStatus {
        summary: latest,
        warnings: vec![warning],
        observed_restart: false,
    })
}

fn service_action_summary(
    action: &str,
    summary: ServiceSummary,
    warnings: Vec<String>,
) -> ServiceActionSummary {
    ServiceActionSummary {
        env_name: summary.env_name,
        service_kind: summary.service_kind,
        action: action.to_string(),
        installed: summary.installed,
        loaded: summary.loaded,
        running: summary.running,
        desired_running: summary.desired_running,
        gateway_port: summary.gateway_port,
        gateway_state: summary.gateway_state,
        gateway_ready: None,
        issue: summary.issue,
        binding_kind: summary.binding_kind,
        binding_name: summary.binding_name,
        stdout_path: summary.stdout_path,
        stderr_path: summary.stderr_path,
        warnings,
    }
}
