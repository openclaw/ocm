use std::path::Path;

use serde::Serialize;

use super::{EnvMeta, EnvironmentService, ExecutionBinding, resolve_execution_binding};
use crate::store::{
    OpenClawConfigRepair, OpenClawRuntimeStateCleanup, audit_openclaw_config, get_launcher,
    get_runtime, prepare_openclaw_config_repair, prepare_openclaw_runtime_state_repair,
    runtime_integrity_issue, save_environment,
};
use crate::store::{OpenClawStateAudit, audit_openclaw_state, derive_env_paths, display_path};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvDoctorSummary {
    pub env_name: String,
    pub root: String,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub healthy: bool,
    pub root_status: String,
    pub config_status: String,
    pub runtime_status: String,
    pub launcher_status: String,
    pub resolution_status: String,
    pub resolved_kind: Option<String>,
    pub resolved_name: Option<String>,
    pub runtime_source_kind: Option<String>,
    pub runtime_release_version: Option<String>,
    pub runtime_release_channel: Option<String>,
    pub issues: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvCleanupActionSummary {
    pub kind: String,
    pub description: String,
    pub applied: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvCleanupSummary {
    pub env_name: String,
    pub root: String,
    pub apply: bool,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub healthy_before: bool,
    pub healthy_after: Option<bool>,
    pub actions: Vec<EnvCleanupActionSummary>,
    pub issues_before: Vec<String>,
    pub issues_after: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvCleanupBatchSummary {
    pub apply: bool,
    pub count: usize,
    pub results: Vec<EnvCleanupSummary>,
}

#[derive(Clone, Debug)]
struct PlannedCleanupAction {
    kind: &'static str,
    description: String,
}

impl<'a> EnvironmentService<'a> {
    pub fn cleanup_preview(&self, name: &str) -> Result<EnvCleanupSummary, String> {
        let env = self.get(name)?;
        let config_audit = audit_openclaw_config(&env, &self.list()?);
        let state_audit = audit_openclaw_state(&env, &self.list()?, self.env);
        if let Some(error) = &state_audit.cleanup_error {
            return Err(error.clone());
        }
        let doctor = self.doctor(name)?;
        let actions = cleanup_actions(&env, &doctor, &config_audit, &state_audit);
        self.prepare_cleanup(&env, &actions)?;
        Ok(build_cleanup_summary(&env, doctor, false, actions, None))
    }

    pub fn cleanup(&self, name: &str) -> Result<EnvCleanupSummary, String> {
        let _operation_lock = self.lock_operation(name)?;
        let mut env = self.get(name)?;
        let config_audit = audit_openclaw_config(&env, &self.list()?);
        let state_audit = audit_openclaw_state(&env, &self.list()?, self.env);
        if let Some(error) = &state_audit.cleanup_error {
            return Err(error.clone());
        }
        let doctor_before = self.doctor(name)?;
        let actions = cleanup_actions(&env, &doctor_before, &config_audit, &state_audit);
        let (config_repair, runtime_cleanup) = self.prepare_cleanup(&env, &actions)?;

        for action in &actions {
            match action.kind {
                "clear-missing-runtime" => {
                    env.default_runtime = None;
                }
                "clear-missing-launcher" => {
                    env.default_launcher = None;
                }
                "repair-openclaw-config" => {
                    if let Some(repair) = &config_repair {
                        repair.apply()?;
                    }
                }
                "reset-openclaw-runtime-state" => {
                    if let Some(cleanup) = &runtime_cleanup {
                        cleanup.apply()?;
                    }
                }
                _ => {}
            }
        }

        if actions.iter().any(|action| {
            matches!(
                action.kind,
                "clear-missing-runtime" | "clear-missing-launcher"
            )
        }) {
            env = save_environment(env, self.env, self.cwd)?;
        } else {
            env = self.get(name)?;
        }

        let doctor_after = self.doctor(name)?;
        Ok(build_cleanup_summary(
            &env,
            doctor_before,
            true,
            actions,
            Some(doctor_after),
        ))
    }

    fn prepare_cleanup(
        &self,
        env: &EnvMeta,
        actions: &[PlannedCleanupAction],
    ) -> Result<
        (
            Option<OpenClawConfigRepair>,
            Option<OpenClawRuntimeStateCleanup>,
        ),
        String,
    > {
        let known_envs = self.list()?;
        let config_repair = if actions
            .iter()
            .any(|action| action.kind == "repair-openclaw-config")
        {
            prepare_openclaw_config_repair(env, &known_envs)?
        } else {
            None
        };
        let runtime_cleanup = if actions
            .iter()
            .any(|action| action.kind == "reset-openclaw-runtime-state")
        {
            let cleanup = prepare_openclaw_runtime_state_repair(
                env,
                self.env,
                config_repair.as_ref().map(|repair| &repair.value),
            )?;
            crate::store::dev_sources::ensure_runtime_cleanup_preserves_dev_sources(
                env,
                &known_envs,
                self,
                cleanup.removals(),
            )?;
            Some(cleanup)
        } else {
            None
        };
        Ok((config_repair, runtime_cleanup))
    }

    pub fn cleanup_all_preview(&self) -> Result<EnvCleanupBatchSummary, String> {
        self.cleanup_all_internal(false)
    }

    pub fn cleanup_all(&self) -> Result<EnvCleanupBatchSummary, String> {
        self.cleanup_all_internal(true)
    }

    pub fn doctor(&self, name: &str) -> Result<EnvDoctorSummary, String> {
        let env = self.get(name)?;
        let env_paths = derive_env_paths(Path::new(&env.root));
        let mut issues = Vec::new();
        let known_envs = self.list()?;

        let root_status = if env_paths.root.exists() {
            "ok".to_string()
        } else {
            push_issue(
                &mut issues,
                format!(
                    "environment root does not exist: {}",
                    display_path(&env_paths.root)
                ),
            );
            "missing".to_string()
        };

        let config_audit = audit_openclaw_config(&env, &known_envs);
        for issue in &config_audit.issues {
            push_issue(&mut issues, issue.clone());
        }
        let config_status = config_audit.status.clone();
        let state_audit = audit_openclaw_state(&env, &known_envs, self.env);
        for issue in &state_audit.issues {
            push_issue(&mut issues, issue.clone());
        }

        let runtime_status = if let Some(runtime_name) = env.default_runtime.clone() {
            match get_runtime(&runtime_name, self.env, self.cwd) {
                Ok(runtime) => match runtime_integrity_issue(&runtime, self.env) {
                    Some(issue) => {
                        push_issue(&mut issues, format!("runtime \"{}\" {issue}", runtime.name));
                        "broken".to_string()
                    }
                    None => "ok".to_string(),
                },
                Err(error) => {
                    push_issue(&mut issues, error);
                    "missing".to_string()
                }
            }
        } else {
            "unbound".to_string()
        };

        let launcher_status = if let Some(launcher_name) = env.default_launcher.clone() {
            match get_launcher(&launcher_name, self.env, self.cwd) {
                Ok(_) => "ok".to_string(),
                Err(error) => {
                    push_issue(&mut issues, error);
                    "missing".to_string()
                }
            }
        } else {
            "unbound".to_string()
        };

        let (
            resolution_status,
            resolved_kind,
            resolved_name,
            runtime_source_kind,
            runtime_release_version,
            runtime_release_channel,
        ) = match resolve_execution_binding(&env, None, None) {
            Ok(ExecutionBinding::Runtime(runtime_name)) => {
                let resolution_status = if runtime_status == "ok" {
                    "ok".to_string()
                } else {
                    "error".to_string()
                };
                let runtime_meta = get_runtime(&runtime_name, self.env, self.cwd).ok();
                (
                    resolution_status,
                    Some("runtime".to_string()),
                    Some(runtime_name),
                    runtime_meta
                        .as_ref()
                        .map(|runtime| runtime.source_kind.as_str().to_string()),
                    runtime_meta
                        .as_ref()
                        .and_then(|runtime| runtime.release_version.clone()),
                    runtime_meta
                        .as_ref()
                        .and_then(|runtime| runtime.release_channel.clone()),
                )
            }
            Ok(ExecutionBinding::Launcher(launcher_name)) => {
                let resolution_status = if launcher_status == "ok" {
                    "ok".to_string()
                } else {
                    "error".to_string()
                };
                (
                    resolution_status,
                    Some("launcher".to_string()),
                    Some(launcher_name),
                    None,
                    None,
                    None,
                )
            }
            Ok(ExecutionBinding::Dev) => (
                "ok".to_string(),
                Some("dev".to_string()),
                Some("dev".to_string()),
                None,
                None,
                None,
            ),
            Err(error) => {
                push_issue(&mut issues, error);
                ("unbound".to_string(), None, None, None, None, None)
            }
        };

        Ok(EnvDoctorSummary {
            env_name: env.name,
            root: env.root,
            default_runtime: env.default_runtime,
            default_launcher: env.default_launcher,
            healthy: issues.is_empty(),
            root_status,
            config_status,
            runtime_status,
            launcher_status,
            resolution_status,
            resolved_kind,
            resolved_name,
            runtime_source_kind,
            runtime_release_version,
            runtime_release_channel,
            issues,
        })
    }

    fn cleanup_all_internal(&self, apply: bool) -> Result<EnvCleanupBatchSummary, String> {
        let env_names = self
            .list()?
            .into_iter()
            .map(|env| env.name)
            .collect::<Vec<_>>();
        let mut results = Vec::new();

        for env_name in env_names {
            let summary = if apply {
                self.cleanup(&env_name)?
            } else {
                self.cleanup_preview(&env_name)?
            };
            if !summary.actions.is_empty() {
                results.push(summary);
            }
        }

        Ok(EnvCleanupBatchSummary {
            apply,
            count: results.len(),
            results,
        })
    }
}

fn push_issue(issues: &mut Vec<String>, issue: String) {
    if !issues.iter().any(|current| current == &issue) {
        issues.push(issue);
    }
}

fn cleanup_actions(
    env: &EnvMeta,
    doctor: &EnvDoctorSummary,
    config_audit: &crate::store::OpenClawConfigAudit,
    state_audit: &OpenClawStateAudit,
) -> Vec<PlannedCleanupAction> {
    let mut actions = Vec::new();

    if doctor.runtime_status == "missing"
        && let Some(runtime_name) = env.default_runtime.as_deref()
    {
        actions.push(PlannedCleanupAction {
            kind: "clear-missing-runtime",
            description: format!("clear missing runtime binding \"{runtime_name}\""),
        });
    }

    if doctor.launcher_status == "missing"
        && let Some(launcher_name) = env.default_launcher.as_deref()
    {
        actions.push(PlannedCleanupAction {
            kind: "clear-missing-launcher",
            description: format!("clear missing launcher binding \"{launcher_name}\""),
        });
    }

    if doctor.config_status == "drifted"
        && (config_audit.repair_source_root.is_some()
            || config_audit.repair_workspace
            || config_audit.repair_gateway_port)
    {
        actions.push(PlannedCleanupAction {
            kind: "repair-openclaw-config",
            description: "rewrite env-scoped OpenClaw config paths and ports".to_string(),
        });
    }

    if state_audit.repair_runtime_state {
        actions.push(PlannedCleanupAction {
            kind: "reset-openclaw-runtime-state",
            description: "clear copied OpenClaw runtime state outside config and workspace"
                .to_string(),
        });
    }

    actions
}

fn build_cleanup_summary(
    env: &EnvMeta,
    doctor_before: EnvDoctorSummary,
    apply: bool,
    actions: Vec<PlannedCleanupAction>,
    doctor_after: Option<EnvDoctorSummary>,
) -> EnvCleanupSummary {
    let actions = actions
        .into_iter()
        .map(|action| EnvCleanupActionSummary {
            kind: action.kind.to_string(),
            description: action.description,
            applied: apply,
        })
        .collect();

    EnvCleanupSummary {
        env_name: env.name.clone(),
        root: env.root.clone(),
        apply,
        default_runtime: env.default_runtime.clone(),
        default_launcher: env.default_launcher.clone(),
        healthy_before: doctor_before.healthy,
        healthy_after: doctor_after.as_ref().map(|doctor| doctor.healthy),
        actions,
        issues_before: doctor_before.issues,
        issues_after: doctor_after.map(|doctor| doctor.issues),
    }
}
