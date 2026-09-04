use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::env::EnvironmentService;
use crate::infra::shell::apply_external_supervision_hint;
use crate::service::inspect::inspect_job;
use crate::service::platform::{
    ManagedServiceDefinition, ManagedServiceEnablement, ServiceManagerKind,
    activate_managed_service, deactivate_managed_service, ensure_service_definition_dir,
    managed_service_enablement, managed_service_identity, restore_managed_service_registration,
    service_manager_kind, validate_managed_service_owner, write_managed_service_definition,
};
use crate::store::{
    display_path, ensure_dir, ensure_store, list_environments, lock_file, now_utc,
    openclaw_port_family_available, openclaw_port_family_range, read_json, resolve_ocm_home,
    supervisor_logs_dir, supervisor_runtime_path, supervisor_state_path, write_json,
};

mod openclaw_handoff;

use openclaw_handoff::{
    RestartHandoffConsumeResult, RestartHandoffSupport, consume_restart_handoff,
    prepare_supervisor_child_spec, probe_restart_handoff_support,
};

const SUPERVISOR_STATE_KIND: &str = "ocm-supervisor-state";
const SUPERVISOR_RUNTIME_KIND: &str = "ocm-supervisor-runtime";
const SUPERVISOR_POLL_INTERVAL_MS: u64 = 200;
const SUPERVISOR_RESTART_DELAY_MS: u64 = 1_000;
const SUPERVISOR_MAX_RESTART_DELAY_MS: u64 = 30_000;
const SUPERVISOR_STABLE_RUN_MS: u64 = 10_000;
const DEFAULT_SERVICE_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";
const SERVICE_PROXY_ENV_KEYS: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
];
const SERVICE_EXTRA_ENV_KEYS: [&str; 2] = ["NODE_EXTRA_CA_CERTS", "NODE_USE_SYSTEM_CA"];
const SUPERVISED_CHILD_BASE_ENV_KEYS: [&str; 5] = ["HOME", "PATH", "OCM_HOME", "OCM_SELF", "SHELL"];
const SUPERVISED_CHILD_RUNTIME_ENV_KEYS: [&str; 4] =
    ["NODE_OPTIONS", "NODE_ENV", "NODE_PATH", "PNPM_HOME"];
const SERVICE_EXECUTABLE_OVERRIDE: &str = "OCM_SERVICE_EXECUTABLE";
pub(crate) const SERVICE_EXECUTABLE_IDENTITY: &str = "ocm-service-supervisor";
const SERVICE_EXECUTABLE_IDENTITY_TIMEOUT_MS: u64 = 1_000;
const SERVICE_ONCE_CHILD_TIMEOUT_MS: u64 = 15_000;
const SERVICE_EXECUTABLE_IDENTITY_BUSY_ATTEMPTS: usize = 5;
const SERVICE_EXECUTABLE_IDENTITY_BUSY_RETRY_MS: u64 = 20;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorChildSpec {
    pub env_name: String,
    pub binding_kind: String,
    pub binding_name: String,
    #[serde(default)]
    pub restart_handoff_pid_bound: bool,
    pub command: Option<String>,
    pub binary_path: Option<String>,
    pub runtime_source_kind: Option<String>,
    pub runtime_release_version: Option<String>,
    pub runtime_release_channel: Option<String>,
    pub args: Vec<String>,
    pub run_dir: String,
    pub child_port: u32,
    pub stdout_path: String,
    pub stderr_path: String,
    pub process_env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedSupervisorEnv {
    pub env_name: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorRestartRequest {
    pub env_name: String,
    pub request_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorState {
    pub kind: String,
    pub ocm_home: String,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub children: Vec<SupervisorChildSpec>,
    pub skipped_envs: Vec<SkippedSupervisorEnv>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restart_requests: Vec<SupervisorRestartRequest>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorView {
    pub state_path: String,
    pub persisted: bool,
    pub kind: String,
    pub ocm_home: String,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub children: Vec<SupervisorChildSpec>,
    pub skipped_envs: Vec<SkippedSupervisorEnv>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorDaemonSummary {
    pub action: String,
    pub managed_label: String,
    pub definition_path: String,
    pub state_path: String,
    pub ocm_home: String,
    pub executable_path: String,
    pub stdout_path: String,
    pub stderr_path: String,
    pub installed: bool,
    pub loaded: bool,
    pub running: bool,
    #[serde(skip)]
    pub(crate) enablement: Option<ManagedServiceEnablement>,
    pub pid: Option<u32>,
    pub state: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorDaemonRefreshSummary {
    #[serde(flatten)]
    pub daemon: SupervisorDaemonSummary,
    pub gateway_interruption_acknowledged: bool,
    pub gateway_interruption_expected: bool,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorChildRunResult {
    pub env_name: String,
    pub binding_kind: String,
    pub binding_name: String,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub restart_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorRunSummary {
    pub state_path: String,
    pub once: bool,
    pub child_count: usize,
    pub stopped_by_signal: bool,
    pub child_results: Vec<SupervisorChildRunResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorRuntimeChild {
    pub env_name: String,
    pub binding_kind: String,
    pub binding_name: String,
    pub pid: u32,
    pub restart_count: usize,
    pub child_port: u32,
    pub stdout_path: String,
    pub stderr_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorRuntimeState {
    pub kind: String,
    pub ocm_home: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub services: Vec<SupervisorRuntimeService>,
    pub children: Vec<SupervisorRuntimeChild>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorRuntimeService {
    pub env_name: String,
    pub binding_kind: String,
    pub binding_name: String,
    pub gateway_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_handoff: Option<String>,
    pub restart_count: usize,
    pub child_port: u32,
    pub pid: Option<u32>,
    pub stdout_path: String,
    pub stderr_path: String,
    pub last_exit_code: Option<i32>,
    pub last_error: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_event_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub next_retry_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorRuntimeView {
    pub runtime_path: String,
    pub present: bool,
    pub kind: String,
    pub ocm_home: String,
    pub daemon_version: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub children: Vec<SupervisorRuntimeChild>,
}

#[derive(Clone, Debug)]
pub struct SupervisorInspection {
    pub daemon: SupervisorDaemonSummary,
    pub daemon_version: Option<String>,
    pub planned_children: Vec<SupervisorChildSpec>,
    pub skipped_envs: Vec<SkippedSupervisorEnv>,
    pub runtime_children: Vec<SupervisorRuntimeChild>,
    pub runtime_services: Vec<SupervisorRuntimeService>,
}

pub struct SupervisorService<'a> {
    env: &'a BTreeMap<String, String>,
    cwd: &'a Path,
}

pub fn sync_supervisor_if_present(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<bool, String> {
    let state_path = supervisor_state_path(env, cwd)?;
    let runtime_path = supervisor_runtime_path(env, cwd)?;
    if !state_path.exists() && !runtime_path.exists() {
        return Ok(false);
    }
    SupervisorService::new(env, cwd).sync()?;
    Ok(true)
}

pub fn sync_supervisor_env_if_present(
    env: &BTreeMap<String, String>,
    cwd: &Path,
    env_name: &str,
) -> Result<bool, String> {
    let state_path = supervisor_state_path(env, cwd)?;
    let runtime_path = supervisor_runtime_path(env, cwd)?;
    if !state_path.exists() && !runtime_path.exists() {
        return Ok(false);
    }
    SupervisorService::new(env, cwd).sync_env(env_name)?;
    Ok(true)
}

pub fn sync_supervisor_binding_if_present(
    env: &BTreeMap<String, String>,
    cwd: &Path,
    binding_kind: &str,
    binding_name: &str,
) -> Result<bool, String> {
    let state_path = supervisor_state_path(env, cwd)?;
    let runtime_path = supervisor_runtime_path(env, cwd)?;
    if !state_path.exists() && !runtime_path.exists() {
        return Ok(false);
    }
    SupervisorService::new(env, cwd).sync_binding(binding_kind, binding_name)?;
    Ok(true)
}

impl<'a> SupervisorService<'a> {
    pub fn new(env: &'a BTreeMap<String, String>, cwd: &'a Path) -> Self {
        Self { env, cwd }
    }

    pub fn plan(&self) -> Result<SupervisorView, String> {
        let state = self.build_state()?;
        let state_path = supervisor_state_path(self.env, self.cwd)?;
        Ok(view_from_state(&state_path, false, state))
    }

    pub fn sync(&self) -> Result<SupervisorView, String> {
        let state_path = supervisor_state_path(self.env, self.cwd)?;
        let _lock = lock_supervisor_state(&state_path)?;
        let mut state = self.build_state()?;
        preserve_persisted_restart_requests(&state_path, &mut state);
        if let Some(parent) = state_path.parent() {
            ensure_dir(parent)?;
        }
        ensure_dir(&supervisor_logs_dir(self.env, self.cwd)?)?;
        write_json(&state_path, &state)?;
        Ok(view_from_state(&state_path, true, state))
    }

    fn sync_env(&self, env_name: &str) -> Result<SupervisorView, String> {
        self.sync_targeted(|_, _| BTreeSet::from([env_name.to_string()]))
    }

    fn sync_binding(
        &self,
        binding_kind: &str,
        binding_name: &str,
    ) -> Result<SupervisorView, String> {
        self.sync_targeted(|fresh_state, persisted_state| {
            fresh_state
                .children
                .iter()
                .chain(
                    persisted_state
                        .into_iter()
                        .flat_map(|state| state.children.iter()),
                )
                .filter(|child| {
                    child.binding_kind == binding_kind && child.binding_name == binding_name
                })
                .map(|child| child.env_name.clone())
                .collect()
        })
    }

    fn sync_targeted(
        &self,
        select_envs: impl FnOnce(&SupervisorState, Option<&SupervisorState>) -> BTreeSet<String>,
    ) -> Result<SupervisorView, String> {
        let state_path = supervisor_state_path(self.env, self.cwd)?;
        let _lock = lock_supervisor_state(&state_path)?;
        let mut state = self.build_state()?;
        let persisted_state = read_json::<SupervisorState>(&state_path).ok();

        if let Some(persisted_state) = persisted_state {
            let refreshed_envs = select_envs(&state, Some(&persisted_state));
            if refreshed_envs.is_empty() {
                return Ok(view_from_state(&state_path, true, persisted_state));
            }
            merge_targeted_supervisor_state(&mut state, persisted_state, &refreshed_envs);
        }

        if let Some(parent) = state_path.parent() {
            ensure_dir(parent)?;
        }
        ensure_dir(&supervisor_logs_dir(self.env, self.cwd)?)?;
        write_json(&state_path, &state)?;
        Ok(view_from_state(&state_path, true, state))
    }

    pub fn runtime(&self) -> Result<SupervisorRuntimeView, String> {
        let runtime_path = supervisor_runtime_path(self.env, self.cwd)?;
        if !runtime_path.exists() {
            let ocm_home = resolve_ocm_home(self.env, self.cwd)?;
            return Ok(SupervisorRuntimeView {
                runtime_path: display_path(&runtime_path),
                present: false,
                kind: SUPERVISOR_RUNTIME_KIND.to_string(),
                ocm_home: display_path(&ocm_home),
                daemon_version: None,
                updated_at: now_utc(),
                children: Vec::new(),
            });
        }

        let runtime = read_json::<SupervisorRuntimeState>(&runtime_path)?;
        Ok(SupervisorRuntimeView {
            runtime_path: display_path(&runtime_path),
            present: true,
            kind: runtime.kind,
            ocm_home: runtime.ocm_home,
            daemon_version: runtime.daemon_version,
            updated_at: runtime.updated_at,
            children: runtime.children,
        })
    }

    pub fn inspect(&self) -> Result<SupervisorInspection, String> {
        let state = self.build_state()?;
        let daemon = self.daemon_status()?;
        let runtime = if daemon.running {
            self.read_runtime_state()?
        } else {
            None
        };

        Ok(SupervisorInspection {
            daemon,
            daemon_version: runtime
                .as_ref()
                .and_then(|runtime| runtime.daemon_version.clone()),
            planned_children: state.children,
            skipped_envs: state.skipped_envs,
            runtime_children: runtime
                .as_ref()
                .map(|runtime| runtime.children.clone())
                .unwrap_or_default(),
            runtime_services: runtime.map(|runtime| runtime.services).unwrap_or_default(),
        })
    }

    pub fn run(&self, once: bool) -> Result<SupervisorRunSummary, String> {
        let (state_path, state) = self.read_persisted_state()?;
        if once {
            return self.run_once(&state_path, state);
        }
        self.run_until_stopped(&state_path, state)
    }

    pub fn install_daemon(&self) -> Result<SupervisorDaemonSummary, String> {
        let _lifecycle_lock = self.lock_daemon_lifecycle()?;
        self.refresh_daemon("install")
    }

    pub fn refresh_daemon_explicit(
        &self,
        acknowledge_gateway_restarts: bool,
    ) -> Result<SupervisorDaemonRefreshSummary, String> {
        let _lifecycle_lock = self.lock_daemon_lifecycle()?;
        let before = self.daemon_status()?;
        if !before.installed {
            return Err(
                "OCM background service is not installed; install an environment service first"
                    .to_string(),
            );
        }

        let gateway_interruption_expected =
            before.running && self.has_desired_running_services()?;
        if gateway_interruption_expected && !acknowledge_gateway_restarts {
            return Err(
                "refreshing the OCM background service interrupts managed gateways; rerun during a maintenance window with --acknowledge-gateway-restarts"
                    .to_string(),
            );
        }

        if before.running {
            self.preserve_operation_owner_before_daemon_refresh()?;
        }

        // Preserve the persisted desired state exactly. Rebuilding it here could
        // apply unrelated latent drift before the daemon handoff.
        let daemon = self.activate_daemon("refresh")?;
        Ok(SupervisorDaemonRefreshSummary {
            daemon,
            gateway_interruption_acknowledged: acknowledge_gateway_restarts,
            gateway_interruption_expected,
            note: if gateway_interruption_expected {
                "OCM background service refreshed; managed gateways were interrupted and will converge from the preserved desired state"
                    .to_string()
            } else {
                "OCM background service refreshed without active managed gateway interruption"
                    .to_string()
            },
        })
    }

    pub fn ensure_daemon_running(&self) -> Result<SupervisorDaemonSummary, String> {
        let _lifecycle_lock = self.lock_daemon_lifecycle()?;
        self.ensure_daemon_running_locked()
    }

    pub(crate) fn ensure_daemon_running_locked(&self) -> Result<SupervisorDaemonSummary, String> {
        let definition = self.supervisor_daemon_definition()?;
        validate_managed_service_owner(&definition, self.env)?;
        let status = self.daemon_status()?;
        if status.running {
            return Ok(status);
        }
        let _ = self.sync()?;
        self.activate_daemon("install")
    }

    pub(crate) fn validate_daemon_owner_locked(&self) -> Result<(), String> {
        let definition = self.supervisor_daemon_definition()?;
        validate_managed_service_owner(&definition, self.env)
    }

    pub(crate) fn restore_daemon_state_locked(
        &self,
        before: &SupervisorDaemonSummary,
    ) -> Result<(), String> {
        let definition = self.supervisor_daemon_definition()?;
        deactivate_managed_service(&definition, self.env)?;
        if before.installed {
            write_managed_service_definition(&definition, self.env)?;
            if before.loaded {
                restore_managed_service_registration(
                    &definition.label,
                    &definition.definition_path,
                    before.running,
                    before.enablement,
                    self.env,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn uninstall_daemon_locked(&self) -> Result<SupervisorDaemonSummary, String> {
        let definition = self.supervisor_daemon_definition()?;
        deactivate_managed_service(&definition, self.env)?;
        self.daemon_summary("uninstall")
    }

    pub(crate) fn lock_daemon_lifecycle(&self) -> Result<crate::store::ExclusiveFileLock, String> {
        let identity = managed_service_identity(self.env, self.cwd)?;
        let definition_dir = identity.definition_path.parent().ok_or_else(|| {
            format!(
                "failed to resolve managed service directory for {}",
                display_path(&identity.definition_path)
            )
        })?;
        // Prepare and validate the manager-owned directory before placing the
        // lifecycle lock in it; lock_file's generic parent creation has no
        // service-directory permission contract.
        ensure_service_definition_dir(definition_dir)?;
        let lock_path = identity.definition_path.with_extension("lifecycle.lock");
        lock_file(&lock_path, "managed service lifecycle")
    }

    pub(crate) fn has_desired_running_services(&self) -> Result<bool, String> {
        Ok(list_environments(self.env, self.cwd)?
            .iter()
            .any(|meta| meta.service_enabled && meta.service_running))
    }

    pub fn recover_child_restart(&self, name: &str) -> Result<SupervisorDaemonSummary, String> {
        self.recover_child_restart_with_request_id(name)
            .map(|(summary, _)| summary)
    }

    pub(crate) fn recover_child_restart_with_request_id(
        &self,
        name: &str,
    ) -> Result<(SupervisorDaemonSummary, String), String> {
        // Reissue only the target request; a full sync could reload unrelated children.
        let request_id = self.request_child_restart(name)?;
        let status = self.daemon_status()?;
        if status.running {
            return Ok((status, request_id));
        }
        self.activate_daemon("install")
            .map(|summary| (summary, request_id))
    }

    pub fn request_child_restart(&self, name: &str) -> Result<String, String> {
        let state_path = supervisor_state_path(self.env, self.cwd)?;
        let _lock = lock_supervisor_state(&state_path)?;
        let mut state = self.build_state()?;
        if let Ok(persisted_state) = read_json::<SupervisorState>(&state_path) {
            preserve_persisted_child_specs_except(&mut state, persisted_state.children, name);
            preserve_restart_requests(&mut state, persisted_state.restart_requests);
        }
        if active_child_spec(&state, name).is_none() {
            return Err(format!(
                "env \"{name}\" is not present in the persisted supervisor state"
            ));
        }

        let request_id = supervisor_restart_request_id(name);
        state
            .restart_requests
            .retain(|request| request.env_name != name);
        state.restart_requests.push(SupervisorRestartRequest {
            env_name: name.to_string(),
            request_id: request_id.clone(),
        });
        if let Some(parent) = state_path.parent() {
            ensure_dir(parent)?;
        }
        ensure_dir(&supervisor_logs_dir(self.env, self.cwd)?)?;
        write_json(&state_path, &state)?;
        Ok(request_id)
    }

    pub fn clear_child_restart_request(&self, name: &str, request_id: &str) -> Result<(), String> {
        let state_path = supervisor_state_path(self.env, self.cwd)?;
        let _lock = lock_supervisor_state(&state_path)?;
        let (state_path, mut state) = self.read_persisted_state()?;
        let previous_len = state.restart_requests.len();
        state
            .restart_requests
            .retain(|request| !(request.env_name == name && request.request_id == request_id));
        if state.restart_requests.len() != previous_len {
            write_json(&state_path, &state)?;
        }
        Ok(())
    }

    pub fn daemon_status(&self) -> Result<SupervisorDaemonSummary, String> {
        self.daemon_summary("status")
    }
    fn build_state(&self) -> Result<SupervisorState, String> {
        ensure_store(self.env, self.cwd)?;
        let ocm_home = resolve_ocm_home(self.env, self.cwd)?;
        let logs_dir = supervisor_logs_dir(self.env, self.cwd)?;
        let env_service = EnvironmentService::new(self.env, self.cwd);
        let mut envs = list_environments(self.env, self.cwd)?;
        envs.sort_by(|left, right| left.name.cmp(&right.name));
        let envs = env_service.apply_effective_gateway_ports(envs)?;

        let mut children = Vec::new();
        let mut skipped_envs = Vec::new();
        for env_meta in envs {
            let name = env_meta.name.clone();
            if !env_meta.service_enabled {
                skipped_envs.push(SkippedSupervisorEnv {
                    env_name: name,
                    reason: "service is disabled".to_string(),
                });
                continue;
            }
            if !env_meta.service_running {
                skipped_envs.push(SkippedSupervisorEnv {
                    env_name: name,
                    reason: "service is stopped".to_string(),
                });
                continue;
            }
            match env_service.resolve_gateway_process(&env_meta.name, false) {
                Ok(process) => {
                    let restart_handoff_pid_bound = process.restart_handoff_pid_bound();
                    let args = process.args.clone();
                    let child_port = process
                        .process_env
                        .get("OPENCLAW_GATEWAY_PORT")
                        .ok_or_else(|| format!("failed to resolve child port for env \"{name}\""))?
                        .parse::<u32>()
                        .map_err(|_| format!("failed to parse child port for env \"{name}\""))?;
                    let process_env = build_supervised_openclaw_env(process.process_env);
                    children.push(SupervisorChildSpec {
                        env_name: name,
                        binding_kind: process.binding_kind,
                        binding_name: process.binding_name,
                        restart_handoff_pid_bound,
                        command: process.command,
                        binary_path: process.binary_path,
                        runtime_source_kind: process.runtime_source_kind,
                        runtime_release_version: process.runtime_release_version,
                        runtime_release_channel: process.runtime_release_channel,
                        args,
                        run_dir: display_path(&process.run_dir),
                        child_port,
                        stdout_path: display_path(
                            &logs_dir.join(format!("{}.stdout.log", process.env_name)),
                        ),
                        stderr_path: display_path(
                            &logs_dir.join(format!("{}.stderr.log", process.env_name)),
                        ),
                        process_env,
                    });
                }
                Err(reason) => skipped_envs.push(SkippedSupervisorEnv {
                    env_name: name,
                    reason,
                }),
            }
        }
        let (children, additional_skipped) = filter_conflicting_supervisor_children(children);
        skipped_envs.extend(additional_skipped);
        skipped_envs.sort_by(|left, right| left.env_name.cmp(&right.env_name));

        Ok(SupervisorState {
            kind: SUPERVISOR_STATE_KIND.to_string(),
            ocm_home: display_path(&ocm_home),
            generated_at: now_utc(),
            children,
            skipped_envs,
            restart_requests: Vec::new(),
        })
    }

    fn read_persisted_state(&self) -> Result<(PathBuf, SupervisorState), String> {
        let state_path = supervisor_state_path(self.env, self.cwd)?;
        if !state_path.exists() {
            return Err(
                "service state has not been written yet; run \"ocm service install <env>\" or \"ocm service start <env>\" first".to_string(),
            );
        }
        let state = read_json(&state_path)?;
        Ok((state_path, state))
    }

    fn refresh_daemon(&self, action: &str) -> Result<SupervisorDaemonSummary, String> {
        let _ = self.sync()?;
        self.activate_daemon(action)
    }

    fn activate_daemon(&self, action: &str) -> Result<SupervisorDaemonSummary, String> {
        let definition = self.supervisor_daemon_definition()?;
        write_managed_service_definition(&definition, self.env)?;
        activate_managed_service(&definition.label, &definition.definition_path, self.env)?;
        self.daemon_summary(action)
    }

    fn preserve_operation_owner_before_daemon_refresh(&self) -> Result<(), String> {
        let Some(active_env) = self.env.get("OCM_ACTIVE_ENV") else {
            return Ok(());
        };
        let managed_gateway_pid = self.read_runtime_state()?.and_then(|runtime| {
            runtime
                .children
                .into_iter()
                .find(|child| child.env_name == *active_env)
                .map(|child| child.pid)
        });
        crate::service::preserve_operation_owner_before_managed_gateway_stop(
            active_env,
            managed_gateway_pid,
            self.env,
            "refreshing the OCM background service",
        )
    }

    fn daemon_summary(&self, action: &str) -> Result<SupervisorDaemonSummary, String> {
        ensure_store(self.env, self.cwd)?;
        let ocm_home = resolve_ocm_home(self.env, self.cwd)?;
        let state_path = supervisor_state_path(self.env, self.cwd)?;
        let identity = managed_service_identity(self.env, self.cwd)?;
        let logs_dir = supervisor_logs_dir(self.env, self.cwd)?;
        let stdout_path = logs_dir.join("daemon.stdout.log");
        let stderr_path = logs_dir.join("daemon.stderr.log");
        let status = inspect_job(&identity.label, &identity.definition_path, self.env);
        let enablement =
            if status.loaded && service_manager_kind(self.env) == ServiceManagerKind::Launchd {
                managed_service_enablement(&identity.label, self.env)?
            } else {
                status.enablement
            };
        let executable_path = self.supervisor_executable_path()?;

        Ok(SupervisorDaemonSummary {
            action: action.to_string(),
            managed_label: identity.label,
            definition_path: display_path(&identity.definition_path),
            state_path: display_path(&state_path),
            ocm_home: display_path(&ocm_home),
            executable_path: display_path(&executable_path),
            stdout_path: display_path(&stdout_path),
            stderr_path: display_path(&stderr_path),
            installed: status.installed,
            loaded: status.loaded,
            running: status.running,
            enablement,
            pid: status.pid,
            state: status.state,
        })
    }

    fn supervisor_daemon_definition(&self) -> Result<ManagedServiceDefinition, String> {
        let ocm_home = resolve_ocm_home(self.env, self.cwd)?;
        let identity = managed_service_identity(self.env, self.cwd)?;
        let logs_dir = supervisor_logs_dir(self.env, self.cwd)?;
        let executable_path = self.supervisor_executable_path()?;

        Ok(ManagedServiceDefinition {
            label: identity.label,
            description: format!(
                "OCM background service for store {}",
                display_path(&ocm_home)
            ),
            definition_path: identity.definition_path,
            program_arguments: vec![
                display_path(&executable_path),
                "__daemon".to_string(),
                "run".to_string(),
            ],
            working_directory: ocm_home.clone(),
            stdout_path: logs_dir.join("daemon.stdout.log"),
            stderr_path: logs_dir.join("daemon.stderr.log"),
            environment: supervisor_service_environment(self.env, &ocm_home, &executable_path),
        })
    }

    fn supervisor_executable_path(&self) -> Result<PathBuf, String> {
        let current_exe = std::env::current_exe().map_err(|error| {
            format!(
                "failed to resolve the current ocm executable for the OCM background service: {error}"
            )
        })?;
        Ok(resolve_supervisor_executable_path(
            self.env,
            self.cwd,
            &current_exe,
        ))
    }

    fn read_runtime_state(&self) -> Result<Option<SupervisorRuntimeState>, String> {
        let runtime_path = supervisor_runtime_path(self.env, self.cwd)?;
        if !runtime_path.exists() {
            return Ok(None);
        }

        Ok(Some(read_json::<SupervisorRuntimeState>(&runtime_path)?))
    }

    fn run_once(
        &self,
        state_path: &Path,
        state: SupervisorState,
    ) -> Result<SupervisorRunSummary, String> {
        let mut child_results = Vec::with_capacity(state.children.len());
        for spec in state.children {
            eprintln!(
                "ocm service: starting {} ({})",
                spec.env_name,
                child_binding_label(&spec)
            );
            let mut child = spawn_supervisor_child(&spec)?;
            let status = wait_child_with_timeout(
                &mut child,
                Duration::from_millis(SERVICE_ONCE_CHILD_TIMEOUT_MS),
                &format!("env \"{}\"", spec.env_name),
            )?;
            child_results.push(child_run_result(&spec, status.code(), 0));
        }

        Ok(SupervisorRunSummary {
            state_path: display_path(state_path),
            once: true,
            child_count: child_results.len(),
            stopped_by_signal: false,
            child_results,
        })
    }

    fn run_until_stopped(
        &self,
        state_path: &Path,
        state: SupervisorState,
    ) -> Result<SupervisorRunSummary, String> {
        let stop_requested = Arc::new(AtomicBool::new(false));
        let signal_flag = Arc::clone(&stop_requested);
        ctrlc::set_handler(move || {
            signal_flag.store(true, Ordering::SeqCst);
        })
        .map_err(|error| format!("failed to install service signal handler: {error}"))?;

        let runtime_path = supervisor_runtime_path(self.env, self.cwd)?;
        let mut active_state = state;
        let mut running = BTreeMap::new();
        let mut pending = BTreeMap::new();
        let mut inactive = BTreeMap::new();
        queue_missing_children(
            &mut pending,
            &running,
            &active_state.children,
            0,
            Instant::now(),
        );
        start_due_children(&mut running, &mut pending, &mut inactive)?;
        write_supervisor_runtime_state(
            &runtime_path,
            &active_state.ocm_home,
            &running,
            &pending,
            &inactive,
        )?;
        let mut managed_child_count = active_state.children.len();
        let mut child_results = Vec::new();

        while !stop_requested.load(Ordering::SeqCst) {
            let mut runtime_dirty = refresh_active_state(
                state_path,
                &mut active_state,
                &mut managed_child_count,
                &mut running,
                &mut pending,
                &mut inactive,
            );
            runtime_dirty |= process_exited_children(
                state_path,
                &stop_requested,
                &mut active_state,
                &mut managed_child_count,
                SupervisorChildCollections {
                    running: &mut running,
                    pending: &mut pending,
                    inactive: &mut inactive,
                    results: &mut child_results,
                },
            )?;

            runtime_dirty |= start_due_children(&mut running, &mut pending, &mut inactive)?;
            if runtime_dirty {
                write_supervisor_runtime_state(
                    &runtime_path,
                    &active_state.ocm_home,
                    &running,
                    &pending,
                    &inactive,
                )?;
            }

            if !stop_requested.load(Ordering::SeqCst) {
                sleep(Duration::from_millis(SUPERVISOR_POLL_INTERVAL_MS));
            }
        }

        for (_, mut running_child) in running {
            stop_supervisor_child(&mut running_child);
        }
        write_supervisor_runtime_state(
            &runtime_path,
            &active_state.ocm_home,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )?;

        Ok(SupervisorRunSummary {
            state_path: display_path(state_path),
            once: false,
            child_count: managed_child_count,
            stopped_by_signal: true,
            child_results,
        })
    }
}

fn lock_supervisor_state(state_path: &Path) -> Result<crate::store::ExclusiveFileLock, String> {
    let lock_path = state_path.with_extension("lock");
    lock_file(&lock_path, "supervisor state")
}

struct RunningSupervisorChild {
    spec: SupervisorChildSpec,
    child: Child,
    restart_count: usize,
    quick_clean_restart_count: usize,
    restart_handoff_support: RestartHandoffSupport,
    started_at: Instant,
}

#[derive(Clone)]
struct PendingSupervisorChild {
    spec: SupervisorChildSpec,
    restart_handoff: Option<String>,
    restart_count: usize,
    quick_clean_restart_count: usize,
    retry_at: Instant,
    retry_at_utc: OffsetDateTime,
    last_exit_code: Option<i32>,
    last_error: Option<String>,
    last_event_at: Option<OffsetDateTime>,
}

struct ExitedSupervisorChild {
    env_name: String,
    exit_code: Option<i32>,
    restart_count: usize,
    quick_clean_restart_count: usize,
    restart_handoff: RestartHandoffConsumeResult,
    ran_for: Duration,
}

struct ExitedSupervisorChildDecision {
    should_restart: bool,
    quick_clean_handoff: bool,
    log_action: &'static str,
    last_error: Option<String>,
}

#[derive(Clone)]
struct InactiveSupervisorChild {
    spec: SupervisorChildSpec,
    gateway_state: String,
    restart_handoff: Option<String>,
    restart_count: usize,
    last_exit_code: Option<i32>,
    last_error: Option<String>,
    last_event_at: Option<OffsetDateTime>,
    next_retry_at: Option<OffsetDateTime>,
}

struct SupervisorChildCollections<'a> {
    running: &'a mut BTreeMap<String, RunningSupervisorChild>,
    pending: &'a mut BTreeMap<String, PendingSupervisorChild>,
    inactive: &'a mut BTreeMap<String, InactiveSupervisorChild>,
    results: &'a mut Vec<SupervisorChildRunResult>,
}

fn spawn_running_child(
    spec: SupervisorChildSpec,
    restart_count: usize,
    quick_clean_restart_count: usize,
) -> Result<RunningSupervisorChild, String> {
    let restart_handoff_support = probe_restart_handoff_support(&spec);
    let prepared_spec = prepare_supervisor_child_spec(&spec, &restart_handoff_support);
    eprintln!(
        "ocm service: starting {} ({})",
        spec.env_name,
        child_binding_label(&spec)
    );
    Ok(RunningSupervisorChild {
        child: spawn_supervisor_child(&prepared_spec)?,
        spec,
        restart_count,
        quick_clean_restart_count,
        restart_handoff_support,
        started_at: Instant::now(),
    })
}

fn spawn_supervisor_child(spec: &SupervisorChildSpec) -> Result<Child, String> {
    let program_arguments = supervisor_program_arguments(spec);
    let Some(program) = program_arguments.first() else {
        return Err(format!(
            "service child env \"{}\" is missing program arguments",
            spec.env_name
        ));
    };
    if let Some(parent) = Path::new(&spec.stdout_path).parent() {
        ensure_dir(parent)?;
    }
    if let Some(parent) = Path::new(&spec.stderr_path).parent() {
        ensure_dir(parent)?;
    }
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.stdout_path)
        .map_err(|error| {
            format!(
                "failed opening stdout log for env \"{}\": {error}",
                spec.env_name
            )
        })?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.stderr_path)
        .map_err(|error| {
            format!(
                "failed opening stderr log for env \"{}\": {error}",
                spec.env_name
            )
        })?;
    let mut process_env = spec.process_env.clone();
    process_env.insert(
        "TMPDIR".to_string(),
        display_path(&prepare_supervisor_child_tmpdir()?),
    );

    let mut command = Command::new(program);
    command
        .args(program_arguments.iter().skip(1))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .env_clear()
        .envs(&process_env)
        .current_dir(Path::new(&spec.run_dir));
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    command.spawn().map_err(|error| {
        format!(
            "failed starting env \"{}\" with {}: {error}",
            spec.env_name,
            child_binding_label(spec)
        )
    })
}

fn wait_child_with_timeout(
    child: &mut Child,
    timeout: Duration,
    label: &str,
) -> Result<ExitStatus, String> {
    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started_at.elapsed() < timeout => {
                sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                terminate_child(child);
                return Err(format!("{label} timed out after {}ms", timeout.as_millis()));
            }
            Err(error) => {
                terminate_child(child);
                return Err(format!("failed waiting for {label}: {error}"));
            }
        }
    }
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let _ = Command::new("kill")
            .args(["-TERM", "--", &process_group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        for _ in 0..20 {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        let _ = Command::new("kill")
            .args(["-KILL", "--", &process_group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn prepare_supervisor_child_tmpdir() -> Result<PathBuf, String> {
    let preferred = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir());
    if let Some(preferred) = preferred
        && let Ok(path) = ensure_supervisor_child_tmpdir(&preferred)
    {
        return Ok(path);
    }

    #[cfg(unix)]
    let fallback = PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let fallback = std::env::temp_dir();
    ensure_supervisor_child_tmpdir(&fallback)
}

fn ensure_supervisor_child_tmpdir(base: &Path) -> Result<PathBuf, String> {
    if !base.is_absolute() || !base.is_dir() {
        return Err(format!(
            "temporary directory base is unavailable: {}",
            display_path(base)
        ));
    }

    let path = base.join(format!("ocm-supervisor-{}", std::process::id()));
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "failed creating supervised child temporary directory {}: {error}",
                display_path(&path)
            ));
        }
    }

    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        format!(
            "failed inspecting supervised child temporary directory {}: {error}",
            display_path(&path)
        )
    })?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "supervised child temporary path is not a directory: {}",
            display_path(&path)
        ));
    }
    #[cfg(unix)]
    {
        let current_uid = unsafe { libc::geteuid() };
        if metadata.uid() != current_uid {
            return Err(format!(
                "supervised child temporary directory is not owned by the current user: {}",
                display_path(&path)
            ));
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "failed securing supervised child temporary directory {}: {error}",
                display_path(&path)
            )
        })?;
    }
    Ok(path)
}

fn supervisor_program_arguments(spec: &SupervisorChildSpec) -> Vec<String> {
    match (&spec.binary_path, &spec.command) {
        (Some(binary_path), _) => {
            let mut program_arguments = vec![binary_path.clone()];
            program_arguments.extend(spec.args.iter().cloned());
            program_arguments
        }
        (None, Some(command)) => {
            if cfg!(windows) {
                vec!["cmd".to_string(), "/C".to_string(), command.to_string()]
            } else {
                vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    command.to_string(),
                ]
            }
        }
        (None, None) => Vec::new(),
    }
}

fn child_binding_label(spec: &SupervisorChildSpec) -> String {
    format!("{}:{}", spec.binding_kind, spec.binding_name)
}

fn child_run_result(
    spec: &SupervisorChildSpec,
    exit_code: Option<i32>,
    restart_count: usize,
) -> SupervisorChildRunResult {
    SupervisorChildRunResult {
        env_name: spec.env_name.clone(),
        binding_kind: spec.binding_kind.clone(),
        binding_name: spec.binding_name.clone(),
        exit_code,
        success: exit_code == Some(0),
        restart_count,
    }
}

fn view_from_state(state_path: &Path, persisted: bool, state: SupervisorState) -> SupervisorView {
    SupervisorView {
        state_path: display_path(state_path),
        persisted,
        kind: state.kind,
        ocm_home: state.ocm_home,
        generated_at: state.generated_at,
        children: state.children,
        skipped_envs: state.skipped_envs,
    }
}

fn child_map(children: &[SupervisorChildSpec]) -> BTreeMap<String, SupervisorChildSpec> {
    children
        .iter()
        .cloned()
        .map(|child| (child.env_name.clone(), child))
        .collect()
}

fn preserve_persisted_restart_requests(state_path: &Path, state: &mut SupervisorState) {
    let Ok(persisted_state) = read_json::<SupervisorState>(state_path) else {
        return;
    };

    preserve_restart_requests(state, persisted_state.restart_requests);
}

fn preserve_persisted_child_specs_except(
    state: &mut SupervisorState,
    persisted_children: Vec<SupervisorChildSpec>,
    refreshed_env_name: &str,
) {
    let persisted = persisted_children
        .into_iter()
        .map(|child| (child.env_name.clone(), child))
        .collect::<BTreeMap<_, _>>();
    for child in &mut state.children {
        if child.env_name == refreshed_env_name {
            continue;
        }
        if let Some(persisted_child) = persisted.get(&child.env_name) {
            *child = persisted_child.clone();
        }
    }
}

fn merge_targeted_supervisor_state(
    fresh_state: &mut SupervisorState,
    persisted_state: SupervisorState,
    refreshed_envs: &BTreeSet<String>,
) {
    let mut children = persisted_state
        .children
        .into_iter()
        .filter(|child| !refreshed_envs.contains(&child.env_name))
        .collect::<Vec<_>>();
    children.extend(
        fresh_state
            .children
            .drain(..)
            .filter(|child| refreshed_envs.contains(&child.env_name)),
    );
    children.sort_by(|left, right| left.env_name.cmp(&right.env_name));
    fresh_state.children = children;

    let mut skipped_envs = persisted_state
        .skipped_envs
        .into_iter()
        .filter(|entry| !refreshed_envs.contains(&entry.env_name))
        .collect::<Vec<_>>();
    skipped_envs.extend(
        fresh_state
            .skipped_envs
            .drain(..)
            .filter(|entry| refreshed_envs.contains(&entry.env_name)),
    );
    skipped_envs.sort_by(|left, right| left.env_name.cmp(&right.env_name));
    fresh_state.skipped_envs = skipped_envs;

    preserve_restart_requests(fresh_state, persisted_state.restart_requests);
}

fn preserve_restart_requests(
    state: &mut SupervisorState,
    requests: impl IntoIterator<Item = SupervisorRestartRequest>,
) {
    for request in requests {
        if active_child_spec(state, &request.env_name).is_none()
            || state.restart_requests.iter().any(|existing| {
                existing.env_name == request.env_name && existing.request_id == request.request_id
            })
        {
            continue;
        }
        state.restart_requests.push(request);
    }
}

fn read_updated_supervisor_state(
    state_path: &Path,
    active_state: &SupervisorState,
) -> Option<SupervisorState> {
    let next_state = match read_json::<SupervisorState>(state_path) {
        Ok(state) => state,
        Err(error) => {
            eprintln!(
                "ocm service: failed reading updated state {}: {}",
                display_path(state_path),
                error
            );
            return None;
        }
    };
    if supervisor_state_equivalent(active_state, &next_state) {
        return None;
    }
    Some(next_state)
}

fn refresh_active_state(
    state_path: &Path,
    active_state: &mut SupervisorState,
    managed_child_count: &mut usize,
    running: &mut BTreeMap<String, RunningSupervisorChild>,
    pending: &mut BTreeMap<String, PendingSupervisorChild>,
    inactive: &mut BTreeMap<String, InactiveSupervisorChild>,
) -> bool {
    let Some(next_state) = read_updated_supervisor_state(state_path, active_state) else {
        return false;
    };
    let restart_requests = new_supervisor_restart_requests(active_state, &next_state);
    let mut runtime_dirty = reconcile_running_children(running, pending, inactive, &next_state);
    runtime_dirty |=
        reconcile_restart_requests(&restart_requests, running, pending, inactive, &next_state);
    *managed_child_count = next_state.children.len();
    *active_state = next_state;
    clear_processed_restart_requests(state_path, &restart_requests);
    runtime_dirty
}

fn reconcile_running_children(
    running: &mut BTreeMap<String, RunningSupervisorChild>,
    pending: &mut BTreeMap<String, PendingSupervisorChild>,
    inactive: &mut BTreeMap<String, InactiveSupervisorChild>,
    desired_state: &SupervisorState,
) -> bool {
    let desired = child_map(&desired_state.children);
    let mut runtime_dirty = false;

    let pending_names = pending.keys().cloned().collect::<Vec<_>>();
    for env_name in pending_names {
        match desired.get(&env_name) {
            Some(next_spec) => {
                let should_replace = pending
                    .get(&env_name)
                    .map(|entry| entry.spec != *next_spec)
                    .unwrap_or(false);
                if should_replace {
                    pending.insert(
                        env_name,
                        pending_supervisor_child(next_spec.clone(), None, 0, 0, 0, None, None),
                    );
                }
            }
            None => {
                let removed_pending = pending.remove(&env_name).is_some();
                let removed_inactive = inactive.remove(&env_name).is_some();
                runtime_dirty |= removed_pending || removed_inactive;
            }
        }
    }

    let active_names = running.keys().cloned().collect::<Vec<_>>();
    for env_name in active_names {
        let Some(next_spec) = desired.get(&env_name) else {
            if let Some(mut existing) = running.remove(&env_name) {
                eprintln!(
                    "ocm service: stopping removed env {}",
                    existing.spec.env_name
                );
                stop_supervisor_child(&mut existing);
                inactive.remove(&env_name);
                runtime_dirty = true;
            }
            continue;
        };
        let needs_restart = running
            .get(&env_name)
            .map(|existing| existing.spec != *next_spec)
            .unwrap_or(false);
        if needs_restart {
            let mut existing = running
                .remove(&env_name)
                .expect("running child should exist when needs_restart is true");
            eprintln!(
                "ocm service: reloading {} ({})",
                existing.spec.env_name,
                child_binding_label(next_spec)
            );
            stop_supervisor_child(&mut existing);
            pending.insert(
                env_name,
                pending_supervisor_child(next_spec.clone(), None, 0, 0, 0, None, None),
            );
            inactive.remove(&existing.spec.env_name);
            runtime_dirty = true;
        }
    }
    queue_missing_children(pending, running, &desired_state.children, 0, Instant::now());
    runtime_dirty
}

fn reconcile_restart_requests(
    restart_requests: &[SupervisorRestartRequest],
    running: &mut BTreeMap<String, RunningSupervisorChild>,
    pending: &mut BTreeMap<String, PendingSupervisorChild>,
    inactive: &mut BTreeMap<String, InactiveSupervisorChild>,
    desired_state: &SupervisorState,
) -> bool {
    if restart_requests.is_empty() {
        return false;
    }

    let desired = child_map(&desired_state.children);
    let mut runtime_dirty = false;
    for request in restart_requests {
        let Some(next_spec) = desired.get(&request.env_name).cloned() else {
            continue;
        };

        if let Some(mut existing) = running.remove(&request.env_name) {
            eprintln!(
                "ocm service: restarting {} ({})",
                existing.spec.env_name,
                child_binding_label(&next_spec)
            );
            let restart_count = existing.restart_count + 1;
            stop_supervisor_child(&mut existing);
            pending.insert(
                request.env_name.clone(),
                pending_supervisor_child(next_spec, None, restart_count, 0, 0, None, None),
            );
            inactive.remove(&request.env_name);
            runtime_dirty = true;
            continue;
        }

        if let Some(pending_child) = pending.get_mut(&request.env_name) {
            pending_child.spec = next_spec;
            pending_child.retry_at = Instant::now();
            pending_child.retry_at_utc = now_utc();
            pending_child.last_error = None;
            pending_child.last_event_at = Some(now_utc());
            inactive.remove(&request.env_name);
            runtime_dirty = true;
            continue;
        }

        pending.insert(
            request.env_name.clone(),
            pending_supervisor_child(next_spec, None, 0, 0, 0, None, None),
        );
        inactive.remove(&request.env_name);
        runtime_dirty = true;
    }
    runtime_dirty
}

fn new_supervisor_restart_requests(
    active_state: &SupervisorState,
    next_state: &SupervisorState,
) -> Vec<SupervisorRestartRequest> {
    next_state
        .restart_requests
        .iter()
        .filter(|request| {
            !active_state.restart_requests.iter().any(|existing| {
                existing.env_name == request.env_name && existing.request_id == request.request_id
            })
        })
        .cloned()
        .collect()
}

fn clear_processed_restart_requests(
    state_path: &Path,
    restart_requests: &[SupervisorRestartRequest],
) {
    if restart_requests.is_empty() {
        return;
    }

    let _lock = match lock_supervisor_state(state_path) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!(
                "ocm service: failed locking state to clear processed restart request {}: {}",
                display_path(state_path),
                error
            );
            return;
        }
    };
    let mut state = match read_json::<SupervisorState>(state_path) {
        Ok(state) => state,
        Err(error) => {
            eprintln!(
                "ocm service: failed reading state to clear processed restart request {}: {}",
                display_path(state_path),
                error
            );
            return;
        }
    };
    let previous_len = state.restart_requests.len();
    state.restart_requests.retain(|request| {
        !restart_requests.iter().any(|processed| {
            processed.env_name == request.env_name && processed.request_id == request.request_id
        })
    });
    if state.restart_requests.len() == previous_len {
        return;
    }
    if let Err(error) = write_json(state_path, &state) {
        eprintln!(
            "ocm service: failed clearing processed restart request {}: {}",
            display_path(state_path),
            error
        );
    }
}

fn collect_exited_children(
    running: &mut BTreeMap<String, RunningSupervisorChild>,
) -> Result<Vec<ExitedSupervisorChild>, String> {
    let mut exited = Vec::new();
    for (env_name, running_child) in running {
        if let Some(status) = running_child.child.try_wait().map_err(|error| {
            format!(
                "failed to poll env \"{}\": {error}",
                running_child.spec.env_name
            )
        })? {
            let ran_for = running_child.started_at.elapsed();
            let restart_handoff = if status.code() == Some(0) {
                consume_restart_handoff(
                    &running_child.spec,
                    &running_child.restart_handoff_support,
                    running_child.child.id(),
                )
            } else {
                RestartHandoffConsumeResult::NotChecked
            };
            exited.push(ExitedSupervisorChild {
                env_name: env_name.clone(),
                exit_code: status.code(),
                restart_count: running_child.restart_count,
                quick_clean_restart_count: running_child.quick_clean_restart_count,
                restart_handoff,
                ran_for,
            });
        }
    }
    Ok(exited)
}

fn process_exited_children(
    state_path: &Path,
    stop_requested: &AtomicBool,
    active_state: &mut SupervisorState,
    managed_child_count: &mut usize,
    children: SupervisorChildCollections<'_>,
) -> Result<bool, String> {
    let SupervisorChildCollections {
        running,
        pending,
        inactive,
        results,
    } = children;
    let exited = collect_exited_children(running)?;
    let mut runtime_dirty = false;

    for exited_child in exited {
        let Some(mut previous_child) = running.remove(&exited_child.env_name) else {
            continue;
        };
        stop_supervisor_child(&mut previous_child);
        runtime_dirty = true;
        results.push(child_run_result(
            &previous_child.spec,
            exited_child.exit_code,
            exited_child.restart_count,
        ));
        runtime_dirty |= refresh_active_state(
            state_path,
            active_state,
            managed_child_count,
            running,
            pending,
            inactive,
        );
        let next_spec = active_child_spec(active_state, &exited_child.env_name).cloned();
        let decision = exited_child_restart_decision(
            &exited_child,
            next_spec
                .as_ref()
                .is_some_and(|spec| *spec == previous_child.spec),
            next_spec.is_some(),
        );
        eprintln!(
            "ocm service: {} exited with {}; {}",
            previous_child.spec.env_name,
            exited_child
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            decision.log_action
        );
        if stop_requested.load(Ordering::SeqCst) {
            break;
        }

        if decision.should_restart
            && let Some(next_spec) = next_spec
        {
            let next_restart_count = if next_spec == previous_child.spec {
                exited_child.restart_count + 1
            } else {
                0
            };
            let next_quick_clean_restart_count =
                if next_spec == previous_child.spec && decision.quick_clean_handoff {
                    exited_child.quick_clean_restart_count + 1
                } else {
                    0
                };
            let issue = decision.last_error.clone();
            pending.insert(
                exited_child.env_name.clone(),
                pending_supervisor_child(
                    next_spec.clone(),
                    Some(
                        previous_child
                            .restart_handoff_support
                            .status_label()
                            .to_string(),
                    ),
                    next_restart_count,
                    next_quick_clean_restart_count,
                    restart_delay_ms(next_restart_count),
                    exited_child.exit_code,
                    issue.clone(),
                ),
            );
            inactive.insert(
                exited_child.env_name,
                inactive_supervisor_child(
                    next_spec,
                    "backoff",
                    Some(
                        previous_child
                            .restart_handoff_support
                            .status_label()
                            .to_string(),
                    ),
                    next_restart_count,
                    exited_child.exit_code,
                    issue,
                    pending
                        .get(&previous_child.spec.env_name)
                        .map(|child| child.retry_at_utc),
                ),
            );
        } else {
            inactive.insert(
                previous_child.spec.env_name.clone(),
                inactive_supervisor_child(
                    previous_child.spec.clone(),
                    "stopped",
                    Some(
                        previous_child
                            .restart_handoff_support
                            .status_label()
                            .to_string(),
                    ),
                    exited_child.restart_count,
                    exited_child.exit_code,
                    decision.last_error.clone(),
                    None,
                ),
            );
        }
    }

    Ok(runtime_dirty)
}

fn exited_child_restart_decision(
    exited_child: &ExitedSupervisorChild,
    same_desired_child: bool,
    desired_child_present: bool,
) -> ExitedSupervisorChildDecision {
    let exit_label = exited_child
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());

    if !desired_child_present {
        return ExitedSupervisorChildDecision {
            should_restart: false,
            quick_clean_handoff: false,
            log_action: "leaving stopped because service is no longer desired running",
            last_error: Some("process exited after service stop; leaving stopped".to_string()),
        };
    }

    if exited_child.exit_code == Some(0) {
        if let RestartHandoffConsumeResult::Unsupported(reason) = &exited_child.restart_handoff {
            return ExitedSupervisorChildDecision {
                should_restart: false,
                quick_clean_handoff: false,
                log_action: "leaving stopped because binding lacks PID-safe external restart handoff",
                last_error: Some(format!(
                    "process exited cleanly but its binding does not support a PID-safe external restart handoff ({reason}); use ocm service restart, bind a direct openclaw.mjs runtime, or upgrade OpenClaw"
                )),
            };
        }
        if let RestartHandoffConsumeResult::Failed(reason) = &exited_child.restart_handoff {
            return ExitedSupervisorChildDecision {
                should_restart: false,
                quick_clean_handoff: false,
                log_action: "leaving stopped because restart handoff could not be verified",
                last_error: Some(format!(
                    "process exited cleanly but OCM could not verify the OpenClaw restart handoff ({reason}); leaving stopped"
                )),
            };
        }
        if let RestartHandoffConsumeResult::NotAccepted(reason) = &exited_child.restart_handoff {
            return ExitedSupervisorChildDecision {
                should_restart: false,
                quick_clean_handoff: false,
                log_action: "leaving stopped without an accepted restart handoff",
                last_error: Some(format!(
                    "process exited cleanly without an accepted OpenClaw restart handoff ({reason}); leaving stopped"
                )),
            };
        }
        if exited_child.restart_handoff != RestartHandoffConsumeResult::Accepted {
            return ExitedSupervisorChildDecision {
                should_restart: false,
                quick_clean_handoff: false,
                log_action: "leaving stopped without a checked restart handoff",
                last_error: Some(
                    "process exited cleanly before OCM checked an OpenClaw restart handoff; leaving stopped"
                        .to_string(),
                ),
            };
        }

        let quick_clean_exit =
            exited_child.ran_for < Duration::from_millis(SUPERVISOR_STABLE_RUN_MS);
        if !same_desired_child || !quick_clean_exit || exited_child.quick_clean_restart_count == 0 {
            return ExitedSupervisorChildDecision {
                should_restart: true,
                quick_clean_handoff: quick_clean_exit,
                log_action: "OpenClaw restart handoff, retrying after backoff",
                last_error: Some(
                    "OpenClaw requested supervisor restart handoff; retrying after backoff"
                        .to_string(),
                ),
            };
        }

        return ExitedSupervisorChildDecision {
            should_restart: false,
            quick_clean_handoff: false,
            log_action: "leaving stopped after repeated quick clean exit",
            last_error: Some(
                "process exited cleanly too quickly after restart handoff; leaving stopped to avoid a restart loop"
                    .to_string(),
            ),
        };
    }

    ExitedSupervisorChildDecision {
        should_restart: true,
        quick_clean_handoff: false,
        log_action: "restarting",
        last_error: Some(format!(
            "process exited with {exit_label}; retrying after backoff"
        )),
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn supervisor_restart_request_id(name: &str) -> String {
    format!("{}-{}-{}", name, std::process::id(), now_millis())
}

fn queue_missing_children(
    pending: &mut BTreeMap<String, PendingSupervisorChild>,
    running: &BTreeMap<String, RunningSupervisorChild>,
    desired_children: &[SupervisorChildSpec],
    restart_count: usize,
    retry_at: Instant,
) {
    for next_spec in desired_children {
        if running.contains_key(&next_spec.env_name) || pending.contains_key(&next_spec.env_name) {
            continue;
        }
        pending.insert(
            next_spec.env_name.clone(),
            PendingSupervisorChild {
                spec: next_spec.clone(),
                restart_handoff: None,
                restart_count,
                quick_clean_restart_count: 0,
                retry_at,
                retry_at_utc: now_utc(),
                last_exit_code: None,
                last_error: None,
                last_event_at: None,
            },
        );
    }
}

fn start_due_children(
    running: &mut BTreeMap<String, RunningSupervisorChild>,
    pending: &mut BTreeMap<String, PendingSupervisorChild>,
    inactive: &mut BTreeMap<String, InactiveSupervisorChild>,
) -> Result<bool, String> {
    let now = Instant::now();
    let due = pending
        .iter()
        .filter_map(|(env_name, child)| (child.retry_at <= now).then_some(env_name.clone()))
        .collect::<Vec<_>>();
    let mut runtime_dirty = false;

    for env_name in due {
        let Some(next_child) = pending.get(&env_name).cloned() else {
            continue;
        };
        if let Err(error) = preflight_supervisor_child_start(&next_child, running) {
            if let Some(entry) = pending.get_mut(&env_name) {
                entry.restart_count += 1;
                let delay_ms = restart_delay_ms(entry.restart_count);
                entry.retry_at = Instant::now() + Duration::from_millis(delay_ms);
                entry.retry_at_utc = now_utc() + time::Duration::milliseconds(delay_ms as i64);
                entry.last_error = Some(error.clone());
                entry.last_event_at = Some(now_utc());
            }
            inactive.insert(
                env_name.clone(),
                inactive_from_pending(
                    pending
                        .get(&env_name)
                        .expect("pending child should still exist after preflight failure"),
                    "backoff",
                ),
            );
            eprintln!("{error}");
            continue;
        }
        match spawn_running_child(
            next_child.spec.clone(),
            next_child.restart_count,
            next_child.quick_clean_restart_count,
        ) {
            Ok(running_child) => {
                pending.remove(&env_name);
                running.insert(env_name.clone(), running_child);
                inactive.remove(&env_name);
                runtime_dirty = true;
            }
            Err(error) => {
                eprintln!("{error}");
                if let Some(entry) = pending.get_mut(&env_name) {
                    entry.restart_count += 1;
                    let delay_ms = restart_delay_ms(entry.restart_count);
                    entry.retry_at = Instant::now() + Duration::from_millis(delay_ms);
                    entry.retry_at_utc = now_utc() + time::Duration::milliseconds(delay_ms as i64);
                    entry.last_error = Some(error.clone());
                    entry.last_event_at = Some(now_utc());
                }
                inactive.insert(
                    env_name.clone(),
                    inactive_from_pending(
                        pending
                            .get(&env_name)
                            .expect("pending child should still exist after spawn failure"),
                        "backoff",
                    ),
                );
            }
        }
    }

    Ok(runtime_dirty)
}

fn stop_supervisor_child(running_child: &mut RunningSupervisorChild) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", running_child.child.id());
        let _ = Command::new("kill")
            .args(["-TERM", "--", &process_group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        for _ in 0..20 {
            let _ = running_child.child.try_wait();
            if !supervisor_process_group_exists(&process_group) {
                break;
            }
            sleep(Duration::from_millis(50));
        }
        if supervisor_process_group_exists(&process_group) {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &process_group])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            for _ in 0..20 {
                let _ = running_child.child.try_wait();
                if !supervisor_process_group_exists(&process_group) {
                    break;
                }
                sleep(Duration::from_millis(50));
            }
        }
    }
    // Always wait on the process-group leader. It can exit after try_wait()
    // reports None but before the group-existence probe; returning in that
    // window leaves a zombie that can block OpenClaw's single-instance lock.
    let _ = running_child.child.kill();
    let _ = running_child.child.wait();
}

#[cfg(unix)]
fn supervisor_process_group_exists(process_group: &str) -> bool {
    Command::new("kill")
        .args(["-0", "--", process_group])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn supervisor_state_equivalent(left: &SupervisorState, right: &SupervisorState) -> bool {
    left.kind == right.kind
        && left.ocm_home == right.ocm_home
        && left.children == right.children
        && left.skipped_envs == right.skipped_envs
        && left.restart_requests == right.restart_requests
}

fn active_child_spec<'a>(
    state: &'a SupervisorState,
    env_name: &str,
) -> Option<&'a SupervisorChildSpec> {
    state
        .children
        .iter()
        .find(|child| child.env_name == env_name)
}

fn filter_conflicting_supervisor_children(
    children: Vec<SupervisorChildSpec>,
) -> (Vec<SupervisorChildSpec>, Vec<SkippedSupervisorEnv>) {
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    let mut claimed_run_dirs = BTreeMap::new();

    for child in children {
        if child.binding_kind != "runtime" {
            if let Some(existing_env) = claimed_run_dirs.get(&child.run_dir) {
                skipped.push(SkippedSupervisorEnv {
                    env_name: child.env_name,
                    reason: format!(
                        "source-backed run dir is already claimed by env \"{}\": {}",
                        existing_env, child.run_dir
                    ),
                });
                continue;
            }
            claimed_run_dirs.insert(child.run_dir.clone(), child.env_name.clone());
        }
        kept.push(child);
    }

    (kept, skipped)
}

fn pending_supervisor_child(
    spec: SupervisorChildSpec,
    restart_handoff: Option<String>,
    restart_count: usize,
    quick_clean_restart_count: usize,
    delay_ms: u64,
    last_exit_code: Option<i32>,
    last_error: Option<String>,
) -> PendingSupervisorChild {
    PendingSupervisorChild {
        spec,
        restart_handoff,
        restart_count,
        quick_clean_restart_count,
        retry_at: Instant::now() + Duration::from_millis(delay_ms),
        retry_at_utc: now_utc() + time::Duration::milliseconds(delay_ms as i64),
        last_exit_code,
        last_error,
        last_event_at: Some(now_utc()),
    }
}

fn inactive_supervisor_child(
    spec: SupervisorChildSpec,
    gateway_state: &str,
    restart_handoff: Option<String>,
    restart_count: usize,
    last_exit_code: Option<i32>,
    last_error: Option<String>,
    next_retry_at: Option<OffsetDateTime>,
) -> InactiveSupervisorChild {
    InactiveSupervisorChild {
        spec,
        gateway_state: gateway_state.to_string(),
        restart_handoff,
        restart_count,
        last_exit_code,
        last_error,
        last_event_at: Some(now_utc()),
        next_retry_at,
    }
}

fn inactive_from_pending(
    pending_child: &PendingSupervisorChild,
    gateway_state: &str,
) -> InactiveSupervisorChild {
    InactiveSupervisorChild {
        spec: pending_child.spec.clone(),
        gateway_state: gateway_state.to_string(),
        restart_handoff: pending_child.restart_handoff.clone(),
        restart_count: pending_child.restart_count,
        last_exit_code: pending_child.last_exit_code,
        last_error: pending_child.last_error.clone(),
        last_event_at: pending_child.last_event_at,
        next_retry_at: Some(pending_child.retry_at_utc),
    }
}

fn restart_delay_ms(restart_count: usize) -> u64 {
    let exponent = restart_count.saturating_sub(1).min(6) as u32;
    SUPERVISOR_RESTART_DELAY_MS
        .saturating_mul(2u64.saturating_pow(exponent))
        .min(SUPERVISOR_MAX_RESTART_DELAY_MS)
}

fn preflight_supervisor_child_start(
    pending_child: &PendingSupervisorChild,
    running: &BTreeMap<String, RunningSupervisorChild>,
) -> Result<(), String> {
    if !openclaw_port_family_available(pending_child.spec.child_port) {
        let (start_port, end_port) = openclaw_port_family_range(pending_child.spec.child_port);
        return Err(format!(
            "refusing to start env \"{}\": OpenClaw port family {}-{} is already in use",
            pending_child.spec.env_name, start_port, end_port
        ));
    }

    let has_run_dir_conflict = running.values().any(|running_child| {
        running_child.spec.run_dir == pending_child.spec.run_dir
            && running_child.spec.binding_kind != "runtime"
    });
    if has_run_dir_conflict {
        return Err(format!(
            "refusing to start env \"{}\": source-backed run dir is already active at {}",
            pending_child.spec.env_name, pending_child.spec.run_dir
        ));
    }

    Ok(())
}

fn supervisor_service_environment(
    process_env: &BTreeMap<String, String>,
    ocm_home: &Path,
    executable_path: &Path,
) -> BTreeMap<String, String> {
    let mut service_env = BTreeMap::new();
    if let Some(home) = process_env
        .get("HOME")
        .filter(|value| !value.trim().is_empty())
    {
        service_env.insert("HOME".to_string(), home.trim().to_string());
    }
    service_env.insert(
        "PATH".to_string(),
        process_env
            .get("PATH")
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|| DEFAULT_SERVICE_PATH.to_string()),
    );
    for key in SERVICE_PROXY_ENV_KEYS {
        if let Some(value) = process_env
            .get(key)
            .filter(|value| !value.trim().is_empty())
        {
            service_env.insert(key.to_string(), value.trim().to_string());
        }
    }
    for key in SERVICE_EXTRA_ENV_KEYS {
        if let Some(value) = process_env
            .get(key)
            .filter(|value| !value.trim().is_empty())
        {
            service_env.insert(key.to_string(), value.trim().to_string());
        }
    }
    service_env.insert("OCM_HOME".to_string(), display_path(ocm_home));
    service_env.insert("OCM_SELF".to_string(), display_path(executable_path));
    service_env
}

fn resolve_supervisor_executable_path(
    env: &BTreeMap<String, String>,
    cwd: &Path,
    current_exe: &Path,
) -> PathBuf {
    if let Some(path) = service_executable_from_override(env, cwd) {
        return path;
    }

    if !is_cargo_dev_executable(current_exe) || is_cargo_test_executable(current_exe) {
        return current_exe.to_path_buf();
    }

    service_executable_from_self(env, cwd)
        .or_else(|| service_executable_from_path(env, cwd, current_exe))
        .unwrap_or_else(|| current_exe.to_path_buf())
}

fn service_executable_from_override(env: &BTreeMap<String, String>, cwd: &Path) -> Option<PathBuf> {
    let path = env_path_value(env, SERVICE_EXECUTABLE_OVERRIDE, cwd)?;
    explicit_service_executable_candidate(path)
}

fn service_executable_from_self(env: &BTreeMap<String, String>, cwd: &Path) -> Option<PathBuf> {
    let path = env_path_value(env, "OCM_SELF", cwd)?;
    discovered_service_executable_candidate(path)
}

fn env_path_value(env: &BTreeMap<String, String>, key: &str, cwd: &Path) -> Option<PathBuf> {
    let value = env.get(key)?.trim();
    if value.is_empty() || !looks_like_path(value) {
        return None;
    }
    let path = PathBuf::from(value);
    Some(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

fn looks_like_path(value: &str) -> bool {
    value.contains('/') || value.contains('\\')
}

fn service_executable_from_path(
    env: &BTreeMap<String, String>,
    cwd: &Path,
    current_exe: &Path,
) -> Option<PathBuf> {
    let path_value = env.get("PATH")?;
    std::env::split_paths(path_value)
        .map(|dir| {
            if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            }
        })
        .map(|dir| dir.join("ocm"))
        .filter(|candidate| candidate != current_exe)
        .find_map(discovered_service_executable_candidate)
}

fn explicit_service_executable_candidate(path: PathBuf) -> Option<PathBuf> {
    is_executable_file(&path).then_some(path)
}

fn discovered_service_executable_candidate(path: PathBuf) -> Option<PathBuf> {
    if !is_executable_file(&path)
        || is_shebang_script(&path)
        || !has_service_executable_identity(&path)
    {
        return None;
    }
    Some(path)
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        metadata.mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn is_shebang_script(path: &Path) -> bool {
    fs::read(path)
        .map(|bytes| bytes.starts_with(b"#!"))
        .unwrap_or(false)
}

fn has_service_executable_identity(path: &Path) -> bool {
    let output = service_executable_identity_output(path);
    let Some(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).trim() == SERVICE_EXECUTABLE_IDENTITY
}

fn service_executable_identity_output(path: &Path) -> Option<Output> {
    let probe_dir = identity_probe_dir()?;
    fs::create_dir_all(&probe_dir).ok()?;
    let output = service_executable_identity_output_in(path, &probe_dir);
    let _ = fs::remove_dir_all(&probe_dir);
    output
}

fn identity_probe_dir() -> Option<PathBuf> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(std::env::temp_dir().join(format!(
        "ocm-service-identity-{}-{unique}",
        std::process::id()
    )))
}

fn service_executable_identity_output_in(path: &Path, probe_dir: &Path) -> Option<Output> {
    service_executable_identity_output_with_spawn(|| {
        Command::new(path)
            .args(["__daemon", "identity"])
            .current_dir(probe_dir)
            .env_clear()
            .env("HOME", probe_dir.join("home"))
            .env("OCM_HOME", probe_dir.join("ocm-home"))
            .env("PATH", DEFAULT_SERVICE_PATH)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    })
}

fn service_executable_identity_output_with_spawn<F>(mut spawn: F) -> Option<Output>
where
    F: FnMut() -> std::io::Result<Child>,
{
    let mut child = spawn_service_executable_identity_child(&mut spawn)?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if started.elapsed()
                    >= Duration::from_millis(SERVICE_EXECUTABLE_IDENTITY_TIMEOUT_MS)
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn spawn_service_executable_identity_child<F>(spawn: &mut F) -> Option<Child>
where
    F: FnMut() -> std::io::Result<Child>,
{
    for attempt in 0..SERVICE_EXECUTABLE_IDENTITY_BUSY_ATTEMPTS {
        match spawn() {
            Ok(child) => return Some(child),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt + 1 < SERVICE_EXECUTABLE_IDENTITY_BUSY_ATTEMPTS =>
            {
                sleep(Duration::from_millis(
                    SERVICE_EXECUTABLE_IDENTITY_BUSY_RETRY_MS,
                ));
            }
            Err(_) => return None,
        }
    }
    None
}

fn is_cargo_dev_executable(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "target")
}

fn is_cargo_test_executable(path: &Path) -> bool {
    let mut saw_target = false;
    let mut saw_deps = false;
    for component in path.components() {
        if component.as_os_str() == "target" {
            saw_target = true;
        } else if saw_target && component.as_os_str() == "deps" {
            saw_deps = true;
        }
    }
    saw_deps
}

fn build_supervised_openclaw_env(
    process_env: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut process_env = stable_supervised_child_env(process_env);
    apply_external_supervision_hint(&mut process_env);
    process_env
}

fn stable_supervised_child_env(process_env: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut stable_env = BTreeMap::new();
    for (key, value) in process_env {
        let value = value.trim();
        if value.is_empty() || !stable_supervised_child_env_key(&key) {
            continue;
        }

        let value = if key == "PATH" {
            stable_supervised_child_path(value).unwrap_or_else(|| DEFAULT_SERVICE_PATH.to_string())
        } else {
            value.to_string()
        };
        stable_env.insert(key, value);
    }
    stable_env
}

fn stable_supervised_child_env_key(key: &str) -> bool {
    key.starts_with("OPENCLAW_")
        || key.starts_with("OCM_")
        || key.starts_with("NPM_CONFIG_")
        || key.starts_with("npm_config_")
        || key.starts_with("COREPACK_")
        || SUPERVISED_CHILD_BASE_ENV_KEYS.contains(&key)
        || SUPERVISED_CHILD_RUNTIME_ENV_KEYS.contains(&key)
        || SERVICE_PROXY_ENV_KEYS.contains(&key)
        || SERVICE_EXTRA_ENV_KEYS.contains(&key)
}

fn stable_supervised_child_path(path: &str) -> Option<String> {
    let mut entries = Vec::new();
    for raw_entry in path.split(':') {
        let entry = raw_entry.trim();
        if entry.is_empty() || volatile_supervised_path_entry(entry) {
            continue;
        }
        if !entries.iter().any(|existing| existing == entry) {
            entries.push(entry.to_string());
        }
    }

    if entries.is_empty() {
        None
    } else {
        Some(entries.join(":"))
    }
}

fn volatile_supervised_path_entry(entry: &str) -> bool {
    let normalized = entry.replace('\\', "/");
    normalized.contains("/codex-home/tmp/")
        || normalized.contains("/.codex/tmp/")
        || normalized.contains("/tmp/arg0/")
        || (normalized.contains("/node_modules/@openai/codex")
            && normalized.ends_with("/codex-path"))
}

fn write_supervisor_runtime_state(
    runtime_path: &Path,
    ocm_home: &str,
    running: &BTreeMap<String, RunningSupervisorChild>,
    pending: &BTreeMap<String, PendingSupervisorChild>,
    inactive: &BTreeMap<String, InactiveSupervisorChild>,
) -> Result<(), String> {
    if let Some(parent) = runtime_path.parent() {
        ensure_dir(parent)?;
    }

    let mut children = running
        .values()
        .map(supervisor_runtime_child)
        .collect::<Vec<_>>();
    children.sort_by(|left, right| left.env_name.cmp(&right.env_name));
    let mut services = running
        .values()
        .map(supervisor_runtime_service_running)
        .collect::<Vec<_>>();
    services.extend(pending.values().map(|child| {
        supervisor_runtime_service_inactive(&inactive_from_pending(child, "backoff"))
    }));
    services.extend(
        inactive
            .values()
            .filter(|child| !pending.contains_key(&child.spec.env_name))
            .map(supervisor_runtime_service_inactive),
    );
    services.sort_by(|left, right| left.env_name.cmp(&right.env_name));

    write_json(
        runtime_path,
        &SupervisorRuntimeState {
            kind: SUPERVISOR_RUNTIME_KIND.to_string(),
            ocm_home: ocm_home.to_string(),
            daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            updated_at: now_utc(),
            services,
            children,
        },
    )
}

fn supervisor_runtime_child(running_child: &RunningSupervisorChild) -> SupervisorRuntimeChild {
    SupervisorRuntimeChild {
        env_name: running_child.spec.env_name.clone(),
        binding_kind: running_child.spec.binding_kind.clone(),
        binding_name: running_child.spec.binding_name.clone(),
        pid: running_child.child.id(),
        restart_count: running_child.restart_count,
        child_port: running_child.spec.child_port,
        stdout_path: running_child.spec.stdout_path.clone(),
        stderr_path: running_child.spec.stderr_path.clone(),
    }
}

fn supervisor_runtime_service_running(
    running_child: &RunningSupervisorChild,
) -> SupervisorRuntimeService {
    SupervisorRuntimeService {
        env_name: running_child.spec.env_name.clone(),
        binding_kind: running_child.spec.binding_kind.clone(),
        binding_name: running_child.spec.binding_name.clone(),
        gateway_state: "running".to_string(),
        restart_handoff: Some(
            running_child
                .restart_handoff_support
                .status_label()
                .to_string(),
        ),
        restart_count: running_child.restart_count,
        child_port: running_child.spec.child_port,
        pid: Some(running_child.child.id()),
        stdout_path: running_child.spec.stdout_path.clone(),
        stderr_path: running_child.spec.stderr_path.clone(),
        last_exit_code: None,
        last_error: None,
        last_event_at: None,
        next_retry_at: None,
    }
}

fn supervisor_runtime_service_inactive(
    inactive_child: &InactiveSupervisorChild,
) -> SupervisorRuntimeService {
    SupervisorRuntimeService {
        env_name: inactive_child.spec.env_name.clone(),
        binding_kind: inactive_child.spec.binding_kind.clone(),
        binding_name: inactive_child.spec.binding_name.clone(),
        gateway_state: inactive_child.gateway_state.clone(),
        restart_handoff: inactive_child.restart_handoff.clone(),
        restart_count: inactive_child.restart_count,
        child_port: inactive_child.spec.child_port,
        pid: None,
        stdout_path: inactive_child.spec.stdout_path.clone(),
        stderr_path: inactive_child.spec.stderr_path.clone(),
        last_exit_code: inactive_child.last_exit_code,
        last_error: inactive_child.last_error.clone(),
        last_event_at: inactive_child.last_event_at,
        next_retry_at: inactive_child.next_retry_at,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn child_spec(name: &str, child_port: u32) -> SupervisorChildSpec {
        SupervisorChildSpec {
            env_name: name.to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: "stable".to_string(),
            restart_handoff_pid_bound: true,
            command: None,
            binary_path: Some("/usr/bin/true".to_string()),
            runtime_source_kind: Some("managed".to_string()),
            runtime_release_version: Some("1.0.0".to_string()),
            runtime_release_channel: Some("stable".to_string()),
            args: Vec::new(),
            run_dir: format!("/tmp/{name}"),
            child_port,
            stdout_path: format!("/tmp/{name}.stdout.log"),
            stderr_path: format!("/tmp/{name}.stderr.log"),
            process_env: BTreeMap::new(),
        }
    }

    fn supervisor_state(restart_requests: Vec<SupervisorRestartRequest>) -> SupervisorState {
        SupervisorState {
            kind: SUPERVISOR_STATE_KIND.to_string(),
            ocm_home: "/tmp/ocm".to_string(),
            generated_at: OffsetDateTime::UNIX_EPOCH,
            children: vec![child_spec("demo", 19999)],
            skipped_envs: Vec::new(),
            restart_requests,
        }
    }

    #[test]
    fn targeted_state_merge_preserves_unrelated_children_skips_and_requests() {
        let mut persisted = supervisor_state(vec![SupervisorRestartRequest {
            env_name: "sibling".to_string(),
            request_id: "restart-sibling".to_string(),
        }]);
        persisted.children.push(child_spec("sibling", 20_001));
        persisted.skipped_envs.push(SkippedSupervisorEnv {
            env_name: "persisted-skipped".to_string(),
            reason: "persisted reason".to_string(),
        });

        let mut fresh = supervisor_state(Vec::new());
        fresh.children[0].runtime_release_version = Some("2.0.0".to_string());
        let mut drifted_sibling = child_spec("sibling", 30_001);
        drifted_sibling.runtime_release_version = Some("latent-drift".to_string());
        fresh.children.push(drifted_sibling);
        fresh.children.push(child_spec("new-unrelated", 30_002));
        fresh.skipped_envs.push(SkippedSupervisorEnv {
            env_name: "new-skipped".to_string(),
            reason: "fresh reason".to_string(),
        });

        merge_targeted_supervisor_state(
            &mut fresh,
            persisted,
            &BTreeSet::from(["demo".to_string()]),
        );

        assert_eq!(fresh.children.len(), 2);
        assert_eq!(
            active_child_spec(&fresh, "demo")
                .unwrap()
                .runtime_release_version,
            Some("2.0.0".to_string())
        );
        let sibling = active_child_spec(&fresh, "sibling").unwrap();
        assert_eq!(sibling.child_port, 20_001);
        assert_eq!(sibling.runtime_release_version, Some("1.0.0".to_string()));
        assert!(active_child_spec(&fresh, "new-unrelated").is_none());
        assert_eq!(
            fresh.skipped_envs,
            vec![SkippedSupervisorEnv {
                env_name: "persisted-skipped".to_string(),
                reason: "persisted reason".to_string(),
            }]
        );
        assert_eq!(
            fresh.restart_requests,
            vec![SupervisorRestartRequest {
                env_name: "sibling".to_string(),
                request_id: "restart-sibling".to_string(),
            }]
        );
    }

    #[test]
    fn targeted_state_merge_can_replace_a_child_with_a_skipped_target() {
        let mut persisted = supervisor_state(vec![
            SupervisorRestartRequest {
                env_name: "demo".to_string(),
                request_id: "restart-demo".to_string(),
            },
            SupervisorRestartRequest {
                env_name: "sibling".to_string(),
                request_id: "restart-sibling".to_string(),
            },
        ]);
        persisted.children.push(child_spec("sibling", 20_001));

        let mut fresh = supervisor_state(Vec::new());
        fresh.children.retain(|child| child.env_name != "demo");
        fresh.children.push(child_spec("sibling", 30_001));
        fresh.skipped_envs.push(SkippedSupervisorEnv {
            env_name: "demo".to_string(),
            reason: "service is stopped".to_string(),
        });

        merge_targeted_supervisor_state(
            &mut fresh,
            persisted,
            &BTreeSet::from(["demo".to_string()]),
        );

        assert!(active_child_spec(&fresh, "demo").is_none());
        assert_eq!(
            active_child_spec(&fresh, "sibling").unwrap().child_port,
            20_001
        );
        assert_eq!(
            fresh.skipped_envs,
            vec![SkippedSupervisorEnv {
                env_name: "demo".to_string(),
                reason: "service is stopped".to_string(),
            }]
        );
        assert_eq!(
            fresh.restart_requests,
            vec![SupervisorRestartRequest {
                env_name: "sibling".to_string(),
                request_id: "restart-sibling".to_string(),
            }]
        );
    }

    fn exited_child(
        exit_code: Option<i32>,
        restart_count: usize,
        ran_for: Duration,
    ) -> ExitedSupervisorChild {
        exited_child_with_clean_count(exit_code, restart_count, 0, ran_for)
    }

    fn exited_child_with_clean_count(
        exit_code: Option<i32>,
        restart_count: usize,
        quick_clean_restart_count: usize,
        ran_for: Duration,
    ) -> ExitedSupervisorChild {
        exited_child_with_handoff_result(
            exit_code,
            restart_count,
            quick_clean_restart_count,
            RestartHandoffConsumeResult::NotAccepted("missing".to_string()),
            ran_for,
        )
    }

    fn exited_child_with_handoff_result(
        exit_code: Option<i32>,
        restart_count: usize,
        quick_clean_restart_count: usize,
        restart_handoff: RestartHandoffConsumeResult,
        ran_for: Duration,
    ) -> ExitedSupervisorChild {
        ExitedSupervisorChild {
            env_name: "demo".to_string(),
            exit_code,
            restart_count,
            quick_clean_restart_count,
            restart_handoff,
            ran_for,
        }
    }

    fn unique_test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ocm-supervisor-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        ))
    }

    fn write_executable_fixture(path: &Path, body: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn service_executable_identity_probe_retries_temporarily_busy_binary() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);

        let output = service_executable_identity_output_with_spawn(|| {
            if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy));
            }

            Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("printf '{}\\n'", SERVICE_EXECUTABLE_IDENTITY))
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
        })
        .unwrap();

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            SERVICE_EXECUTABLE_IDENTITY
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn service_executable_identity_probe_rejects_non_busy_spawn_errors() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);

        let output = service_executable_identity_output_with_spawn(|| {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });

        assert!(output.is_none());
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn service_executable_identity_probe_stops_after_busy_retry_limit() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);

        let output = service_executable_identity_output_with_spawn(|| {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(std::io::Error::from(std::io::ErrorKind::ExecutableFileBusy))
        });

        assert!(output.is_none());
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            SERVICE_EXECUTABLE_IDENTITY_BUSY_ATTEMPTS
        );
    }

    #[test]
    fn supervisor_executable_rejects_unidentified_path_ocm_for_dev_artifacts() {
        let root = unique_test_root("unidentified-path-ocm");
        let installed_dir = root.join("installed-bin");
        let installed_ocm = installed_dir.join("ocm");
        write_executable_fixture(&installed_ocm, b"binary");

        let current_exe = root.join("repo/target/bin-ocm/debug/ocm");
        write_executable_fixture(&current_exe, b"dev-binary");
        let wrapper = root.join("repo/bin/ocm");
        write_executable_fixture(&wrapper, b"#!/bin/sh\ncargo run \"$@\"\n");

        let env = BTreeMap::from([
            ("PATH".to_string(), display_path(&installed_dir)),
            ("OCM_SELF".to_string(), display_path(&wrapper)),
        ]);

        assert_eq!(
            resolve_supervisor_executable_path(&env, &root, &current_exe),
            current_exe
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn supervisor_executable_rejects_unidentified_relative_path_ocm() {
        let root = unique_test_root("relative-path-ocm");
        let cwd = root.join("repo");
        fs::create_dir_all(&cwd).unwrap();
        let installed_ocm = cwd.join("target/release/ocm");
        write_executable_fixture(&installed_ocm, b"binary");

        let current_exe = cwd.join("target/bin-ocm/debug/ocm");
        write_executable_fixture(&current_exe, b"dev-binary");
        let env = BTreeMap::from([("PATH".to_string(), "target/release".to_string())]);

        assert_eq!(
            resolve_supervisor_executable_path(&env, &cwd, &current_exe),
            current_exe
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn supervisor_executable_keeps_cargo_test_binary() {
        let root = unique_test_root("test-binary");
        let installed_dir = root.join("installed-bin");
        let installed_ocm = installed_dir.join("ocm");
        write_executable_fixture(&installed_ocm, b"binary");

        let current_exe = root.join("repo/target/debug/deps/daemon_runtime_tests-123");
        write_executable_fixture(&current_exe, b"test-binary");
        let env = BTreeMap::from([("PATH".to_string(), display_path(&installed_dir))]);

        assert_eq!(
            resolve_supervisor_executable_path(&env, &root, &current_exe),
            current_exe
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn supervisor_executable_accepts_explicit_binary_override() {
        let root = unique_test_root("override");
        let override_path = root.join("custom/ocm");
        write_executable_fixture(&override_path, b"binary");

        let current_exe = root.join("repo/target/bin-ocm/debug/ocm");
        write_executable_fixture(&current_exe, b"dev-binary");
        let env = BTreeMap::from([(
            SERVICE_EXECUTABLE_OVERRIDE.to_string(),
            display_path(&override_path),
        )]);

        assert_eq!(
            resolve_supervisor_executable_path(&env, &root, &current_exe),
            override_path
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn supervisor_executable_keeps_release_binary() {
        let root = unique_test_root("release");
        let installed_dir = root.join("other-bin");
        let other_ocm = installed_dir.join("ocm");
        write_executable_fixture(&other_ocm, b"binary");

        let current_exe = root.join(".local/bin/ocm");
        write_executable_fixture(&current_exe, b"release-binary");
        let env = BTreeMap::from([("PATH".to_string(), display_path(&installed_dir))]);

        assert_eq!(
            resolve_supervisor_executable_path(&env, &root, &current_exe),
            current_exe
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn desired_quick_clean_exit_with_handoff_restarts() {
        let decision = exited_child_restart_decision(
            &exited_child_with_handoff_result(
                Some(0),
                0,
                0,
                RestartHandoffConsumeResult::Accepted,
                Duration::from_millis(50),
            ),
            true,
            true,
        );

        assert!(decision.should_restart);
        assert_eq!(
            decision.log_action,
            "OpenClaw restart handoff, retrying after backoff"
        );
        assert!(
            decision
                .last_error
                .unwrap()
                .contains("supervisor restart handoff")
        );
    }

    #[test]
    fn desired_stable_clean_exit_with_handoff_restarts() {
        let decision = exited_child_restart_decision(
            &exited_child_with_handoff_result(
                Some(0),
                0,
                1,
                RestartHandoffConsumeResult::Accepted,
                Duration::from_millis(SUPERVISOR_STABLE_RUN_MS + 1),
            ),
            true,
            true,
        );

        assert!(decision.should_restart);
        assert!(!decision.quick_clean_handoff);
    }

    #[test]
    fn desired_quick_clean_exit_without_accepted_handoff_stays_stopped() {
        let decision = exited_child_restart_decision(
            &exited_child(Some(0), 0, Duration::from_millis(50)),
            true,
            true,
        );

        assert!(!decision.should_restart);
        assert!(
            decision
                .last_error
                .unwrap()
                .contains("without an accepted OpenClaw restart handoff")
        );
    }

    #[test]
    fn unsupported_binding_clean_exit_reports_safe_restart_guidance() {
        let decision = exited_child_restart_decision(
            &exited_child_with_handoff_result(
                Some(0),
                0,
                0,
                RestartHandoffConsumeResult::Unsupported(
                    "runtime does not advertise protocol version 1".to_string(),
                ),
                Duration::from_millis(SUPERVISOR_STABLE_RUN_MS + 1),
            ),
            true,
            true,
        );

        assert!(!decision.should_restart);
        let error = decision.last_error.unwrap();
        assert!(error.contains("does not support a PID-safe external restart handoff"));
        assert!(error.contains("bind a direct openclaw.mjs runtime"));
        assert!(error.contains("upgrade OpenClaw"));
        assert!(error.contains("ocm service restart"));
    }

    #[test]
    fn quick_clean_exit_after_service_stop_stays_stopped() {
        let decision = exited_child_restart_decision(
            &exited_child(Some(0), 0, Duration::from_millis(50)),
            false,
            false,
        );

        assert!(!decision.should_restart);
        assert!(decision.last_error.unwrap().contains("after service stop"));
    }

    #[test]
    fn repeated_quick_clean_exit_stays_stopped() {
        let decision = exited_child_restart_decision(
            &exited_child_with_handoff_result(
                Some(0),
                1,
                1,
                RestartHandoffConsumeResult::Accepted,
                Duration::from_millis(50),
            ),
            true,
            true,
        );

        assert!(!decision.should_restart);
        assert!(
            decision
                .last_error
                .unwrap()
                .contains("avoid a restart loop")
        );
    }

    #[test]
    fn previous_crash_retry_does_not_consume_clean_handoff_allowance() {
        let decision = exited_child_restart_decision(
            &exited_child_with_handoff_result(
                Some(0),
                3,
                0,
                RestartHandoffConsumeResult::Accepted,
                Duration::from_millis(50),
            ),
            true,
            true,
        );

        assert!(decision.should_restart);
        assert!(decision.quick_clean_handoff);
    }

    #[test]
    fn nonzero_exit_still_restarts() {
        let decision = exited_child_restart_decision(
            &exited_child(Some(1), 0, Duration::from_millis(50)),
            true,
            true,
        );

        assert!(decision.should_restart);
        assert_eq!(
            decision.last_error.unwrap(),
            "process exited with 1; retrying after backoff"
        );
    }

    #[cfg(unix)]
    #[test]
    fn exited_supervisor_child_cleans_remaining_process_group() {
        let test_dir = std::env::temp_dir().join(format!(
            "ocm-supervisor-process-group-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&test_dir).unwrap();
        let descendant_pid_path = test_dir.join("descendant.pid");
        let mut spec = child_spec("process-group-cleanup", 19_999);
        spec.command = Some(
            "trap '' HUP; sleep 60 & descendant=$!; printf '%s' \"$descendant\" > \"$OCM_TEST_DESCENDANT_PID_FILE\"; exit 1"
                .to_string(),
        );
        spec.binary_path = None;
        spec.run_dir = test_dir.to_string_lossy().into_owned();
        spec.stdout_path = test_dir.join("stdout.log").to_string_lossy().into_owned();
        spec.stderr_path = test_dir.join("stderr.log").to_string_lossy().into_owned();
        spec.process_env.insert(
            "OCM_TEST_DESCENDANT_PID_FILE".to_string(),
            descendant_pid_path.to_string_lossy().into_owned(),
        );

        let mut running_child = spawn_running_child(spec, 0, 0).unwrap();
        let process_group = format!("-{}", running_child.child.id());
        let status = running_child.child.wait().unwrap();
        assert_eq!(status.code(), Some(1));

        for _ in 0..100 {
            if descendant_pid_path.exists() {
                break;
            }
            sleep(Duration::from_millis(10));
        }
        let descendant_pid = fs::read_to_string(&descendant_pid_path).unwrap();
        assert!(supervisor_process_group_exists(&process_group));

        stop_supervisor_child(&mut running_child);

        for _ in 0..100 {
            let descendant_alive = Command::new("kill")
                .args(["-0", "--", descendant_pid.trim()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !descendant_alive {
                break;
            }
            sleep(Duration::from_millis(10));
        }
        assert!(!supervisor_process_group_exists(&process_group));
        let _ = fs::remove_dir_all(test_dir);
    }

    #[cfg(unix)]
    #[test]
    fn stopped_supervisor_child_reaps_process_group_leader() {
        let test_dir = std::env::temp_dir().join(format!(
            "ocm-supervisor-child-reap-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&test_dir).unwrap();
        let mut spec = child_spec("process-group-leader-reap", 19_998);
        spec.command = Some("trap 'exit 0' TERM; while :; do sleep 1; done".to_string());
        spec.binary_path = None;
        spec.run_dir = test_dir.to_string_lossy().into_owned();
        spec.stdout_path = test_dir.join("stdout.log").to_string_lossy().into_owned();
        spec.stderr_path = test_dir.join("stderr.log").to_string_lossy().into_owned();

        let mut running_child = spawn_running_child(spec, 0, 0).unwrap();
        let child_pid = running_child.child.id().to_string();
        sleep(Duration::from_millis(50));

        stop_supervisor_child(&mut running_child);

        let child_still_exists = Command::new("kill")
            .args(["-0", "--", &child_pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(!child_still_exists, "supervised child was not reaped");
        let _ = fs::remove_dir_all(test_dir);
    }

    #[test]
    fn restart_requests_are_part_of_supervisor_state_equivalence() {
        let active = supervisor_state(Vec::new());
        let requested = supervisor_state(vec![SupervisorRestartRequest {
            env_name: "demo".to_string(),
            request_id: "restart-1".to_string(),
        }]);

        assert!(!supervisor_state_equivalent(&active, &requested));
        assert!(supervisor_state_equivalent(&requested, &requested));
    }

    #[test]
    fn new_restart_requests_only_include_unseen_ids() {
        let active = supervisor_state(vec![SupervisorRestartRequest {
            env_name: "demo".to_string(),
            request_id: "restart-1".to_string(),
        }]);
        let next = supervisor_state(vec![
            SupervisorRestartRequest {
                env_name: "demo".to_string(),
                request_id: "restart-1".to_string(),
            },
            SupervisorRestartRequest {
                env_name: "demo".to_string(),
                request_id: "restart-2".to_string(),
            },
        ]);

        let requests = new_supervisor_restart_requests(&active, &next);

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].request_id, "restart-2");
    }

    #[test]
    fn preserve_restart_requests_keeps_only_active_children() {
        let mut state = supervisor_state(Vec::new());
        preserve_restart_requests(
            &mut state,
            [
                SupervisorRestartRequest {
                    env_name: "demo".to_string(),
                    request_id: "restart-1".to_string(),
                },
                SupervisorRestartRequest {
                    env_name: "missing".to_string(),
                    request_id: "restart-2".to_string(),
                },
            ],
        );

        assert_eq!(
            state.restart_requests,
            vec![SupervisorRestartRequest {
                env_name: "demo".to_string(),
                request_id: "restart-1".to_string(),
            }]
        );
    }

    #[test]
    fn restart_request_requeues_only_target_pending_child() {
        let next_rescue = child_spec("rescue", 18790);
        let main = child_spec("main", 19123);
        let state = SupervisorState {
            kind: SUPERVISOR_STATE_KIND.to_string(),
            ocm_home: "/tmp/ocm".to_string(),
            generated_at: OffsetDateTime::UNIX_EPOCH,
            children: vec![main.clone(), next_rescue.clone()],
            skipped_envs: Vec::new(),
            restart_requests: Vec::new(),
        };
        let mut running = BTreeMap::new();
        let mut pending = BTreeMap::from([
            (
                "main".to_string(),
                pending_supervisor_child(
                    main.clone(),
                    Some("protocol-v1".to_string()),
                    7,
                    0,
                    30_000,
                    Some(1),
                    Some("backoff".to_string()),
                ),
            ),
            (
                "rescue".to_string(),
                pending_supervisor_child(
                    child_spec("rescue", 11111),
                    Some("legacy".to_string()),
                    5,
                    0,
                    30_000,
                    Some(1),
                    Some("backoff".to_string()),
                ),
            ),
        ]);
        let mut inactive = BTreeMap::new();

        let runtime_dirty = reconcile_restart_requests(
            &[SupervisorRestartRequest {
                env_name: "rescue".to_string(),
                request_id: "restart-1".to_string(),
            }],
            &mut running,
            &mut pending,
            &mut inactive,
            &state,
        );

        assert!(runtime_dirty);
        assert_eq!(pending.get("rescue").unwrap().spec, next_rescue);
        assert_eq!(pending.get("main").unwrap().spec, main);
        assert_eq!(pending.get("main").unwrap().restart_count, 7);
    }

    #[test]
    fn removing_crash_looping_child_marks_runtime_state_dirty() {
        let target = child_spec("rosita", 18790);
        let control = child_spec("control", 19123);
        let desired = SupervisorState {
            kind: SUPERVISOR_STATE_KIND.to_string(),
            ocm_home: "/tmp/ocm".to_string(),
            generated_at: OffsetDateTime::UNIX_EPOCH,
            children: vec![control.clone()],
            skipped_envs: Vec::new(),
            restart_requests: Vec::new(),
        };
        let mut running = BTreeMap::new();
        let mut pending = BTreeMap::from([
            (
                target.env_name.clone(),
                pending_supervisor_child(
                    target.clone(),
                    None,
                    4,
                    0,
                    30_000,
                    Some(1),
                    Some("schema 8 is newer than supported schema 7".to_string()),
                ),
            ),
            (
                control.env_name.clone(),
                pending_supervisor_child(
                    control.clone(),
                    None,
                    2,
                    0,
                    30_000,
                    Some(1),
                    Some("control backoff".to_string()),
                ),
            ),
        ]);
        let mut inactive = pending
            .iter()
            .map(|(name, child)| (name.clone(), inactive_from_pending(child, "backoff")))
            .collect::<BTreeMap<_, _>>();

        let runtime_dirty =
            reconcile_running_children(&mut running, &mut pending, &mut inactive, &desired);

        assert!(runtime_dirty, "published runtime state must drop rosita");
        assert!(!pending.contains_key("rosita"));
        assert!(!inactive.contains_key("rosita"));
        assert_eq!(pending.get("control").unwrap().spec, control);
        assert!(inactive.contains_key("control"));
    }

    #[test]
    fn supervised_child_env_does_not_expose_the_shared_supervisor_identity() {
        let process_env = build_supervised_openclaw_env(BTreeMap::from([
            (
                "OPENCLAW_LAUNCHD_LABEL".to_string(),
                "ai.openclaw.ocm".to_string(),
            ),
            (
                "OPENCLAW_SYSTEMD_UNIT".to_string(),
                "ai.openclaw.ocm.service".to_string(),
            ),
            (
                "OPENCLAW_WINDOWS_TASK_NAME".to_string(),
                "OCM Supervisor".to_string(),
            ),
        ]));

        assert_eq!(
            process_env
                .get("OPENCLAW_SERVICE_MARKER")
                .map(String::as_str),
            Some("openclaw")
        );
        assert!(!process_env.contains_key("OPENCLAW_SERVICE_KIND"));
        assert_eq!(
            process_env
                .get("OPENCLAW_SUPERVISOR_MODE")
                .map(String::as_str),
            Some("external")
        );
        assert_eq!(
            process_env.get("OPENCLAW_NO_RESPAWN").map(String::as_str),
            Some("1")
        );
        assert!(!process_env.contains_key("OPENCLAW_LAUNCHD_LABEL"));
        assert!(!process_env.contains_key("OPENCLAW_SYSTEMD_UNIT"));
        assert!(!process_env.contains_key("OPENCLAW_WINDOWS_TASK_NAME"));
    }

    #[test]
    fn supervised_child_env_drops_volatile_caller_context() {
        let process_env = build_supervised_openclaw_env(BTreeMap::from([
            ("HOME".to_string(), "/Users/demo".to_string()),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("TMPDIR".to_string(), "/tmp/demo".to_string()),
            ("OCM_HOME".to_string(), "/Users/demo/.ocm".to_string()),
            ("OCM_ACTIVE_ENV".to_string(), "rescue".to_string()),
            (
                "OPENCLAW_HOME".to_string(),
                "/Users/demo/.ocm/envs/rescue/.openclaw".to_string(),
            ),
            ("HTTPS_PROXY".to_string(), "http://proxy".to_string()),
            ("GH_TOKEN".to_string(), "caller-token".to_string()),
            ("PWD".to_string(), "/tmp/caller-workspace".to_string()),
            (
                "XPC_SERVICE_NAME".to_string(),
                "agent-heartbeat".to_string(),
            ),
            ("CODEX_SESSION_ID".to_string(), "session-123".to_string()),
        ]));

        assert_eq!(
            process_env.get("HOME").map(String::as_str),
            Some("/Users/demo")
        );
        assert_eq!(
            process_env.get("OPENCLAW_HOME").map(String::as_str),
            Some("/Users/demo/.ocm/envs/rescue/.openclaw")
        );
        assert_eq!(
            process_env.get("OCM_ACTIVE_ENV").map(String::as_str),
            Some("rescue")
        );
        assert_eq!(
            process_env.get("HTTPS_PROXY").map(String::as_str),
            Some("http://proxy")
        );
        assert_eq!(
            process_env.get("PATH").map(String::as_str),
            Some("/usr/bin:/bin")
        );
        assert!(!process_env.contains_key("TMPDIR"));
        assert!(!process_env.contains_key("GH_TOKEN"));
        assert!(!process_env.contains_key("PWD"));
        assert!(!process_env.contains_key("XPC_SERVICE_NAME"));
        assert!(!process_env.contains_key("CODEX_SESSION_ID"));
    }

    #[test]
    fn supervised_child_env_sanitizes_volatile_path_entries() {
        let process_env = build_supervised_openclaw_env(BTreeMap::from([
                ("HOME".to_string(), "/Users/demo".to_string()),
                (
                    "PATH".to_string(),
                    [
                        "/opt/homebrew/bin",
                        "/usr/bin",
                        "/bin",
                        "/usr/bin",
                        "/Users/demo/.ocm/envs/rescue/.openclaw/agents/main/agent/codex-home/tmp/arg0/codex-arg0abc",
                        "/Users/demo/.codex/tmp/arg0/codex-arg0def",
                        "/Users/demo/.ocm/envs/rescue/.openclaw/npm/projects/openclaw-codex/node_modules/@openclaw/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/codex-path",
                        "/opt/homebrew/Cellar/node/26.3.0/bin",
                        "/opt/homebrew/Cellar/node@22/22.17.0/bin",
                        "/Users/demo/.local/bin",
                    ]
                    .join(":"),
                ),
            ]));

        assert_eq!(
            process_env.get("PATH").map(String::as_str),
            Some(
                "/opt/homebrew/bin:/usr/bin:/bin:/opt/homebrew/Cellar/node/26.3.0/bin:/opt/homebrew/Cellar/node@22/22.17.0/bin:/Users/demo/.local/bin"
            )
        );
    }

    #[test]
    fn supervised_child_env_preserves_stable_runtime_tool_env() {
        let process_env = build_supervised_openclaw_env(BTreeMap::from([
            (
                "NODE_OPTIONS".to_string(),
                "--max-old-space-size=4096".to_string(),
            ),
            ("NODE_ENV".to_string(), "production".to_string()),
            (
                "NPM_CONFIG_REGISTRY".to_string(),
                "https://registry.example".to_string(),
            ),
            ("npm_config_cache".to_string(), "/tmp/npm-cache".to_string()),
            (
                "COREPACK_HOME".to_string(),
                "/Users/demo/.cache/corepack".to_string(),
            ),
            (
                "PNPM_HOME".to_string(),
                "/Users/demo/.local/share/pnpm".to_string(),
            ),
            ("GH_TOKEN".to_string(), "caller-token".to_string()),
            ("PWD".to_string(), "/tmp/caller-workspace".to_string()),
        ]));

        assert_eq!(
            process_env.get("NODE_OPTIONS").map(String::as_str),
            Some("--max-old-space-size=4096")
        );
        assert_eq!(
            process_env.get("NODE_ENV").map(String::as_str),
            Some("production")
        );
        assert_eq!(
            process_env.get("NPM_CONFIG_REGISTRY").map(String::as_str),
            Some("https://registry.example")
        );
        assert_eq!(
            process_env.get("npm_config_cache").map(String::as_str),
            Some("/tmp/npm-cache")
        );
        assert_eq!(
            process_env.get("COREPACK_HOME").map(String::as_str),
            Some("/Users/demo/.cache/corepack")
        );
        assert_eq!(
            process_env.get("PNPM_HOME").map(String::as_str),
            Some("/Users/demo/.local/share/pnpm")
        );
        assert!(!process_env.contains_key("GH_TOKEN"));
        assert!(!process_env.contains_key("PWD"));
    }

    #[cfg(unix)]
    #[test]
    fn wait_child_with_timeout_kills_sleep_after_deadline() {
        let started = Instant::now();
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn /bin/sleep 30");
        let pid = child.id();
        let error = wait_child_with_timeout(&mut child, Duration::from_millis(200), "sleep")
            .expect_err("sleep should be killed at the deadline");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "timed wait should return well before the 30s sleep, took {elapsed:?}"
        );
        assert!(
            error.contains("timed out"),
            "expected timeout error, got {error}"
        );
        let still_alive = Command::new("kill")
            .args(["-0", "--", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(!still_alive, "child {pid} should be dead after timeout");
    }

    #[cfg(unix)]
    #[test]
    fn wait_child_with_timeout_returns_status_when_child_exits() {
        let mut child = Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let status = wait_child_with_timeout(&mut child, Duration::from_millis(200), "true")
            .expect("exited child should not time out");
        assert!(status.success());
    }
}
