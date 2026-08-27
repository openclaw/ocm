use serde::Serialize;

use super::{
    EnvironmentService, ExecutionBinding, resolve_execution_binding, resolve_runtime_run_dir,
};
use crate::launcher::resolve_launcher_run_dir;
use crate::service::ServiceService;
use crate::store::{get_launcher, runtime_operational_issue};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvStatusSummary {
    pub env_name: String,
    pub root: String,
    pub gateway_port: Option<u32>,
    pub gateway_port_source: Option<String>,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub resolved_kind: Option<String>,
    pub resolved_name: Option<String>,
    pub binary_path: Option<String>,
    pub command: Option<String>,
    pub run_dir: Option<String>,
    pub runtime_source_kind: Option<String>,
    pub runtime_release_version: Option<String>,
    pub runtime_release_channel: Option<String>,
    pub runtime_health: Option<String>,
    pub managed_service_state: Option<String>,
    pub openclaw_state: Option<String>,
    pub global_service_state: Option<String>,
    pub service_definition_drift: Option<bool>,
    pub service_live_exec_unverified: Option<bool>,
    pub service_orphaned_live: Option<bool>,
    pub service_issue: Option<String>,
    pub issue: Option<String>,
}

impl<'a> EnvironmentService<'a> {
    pub fn status(&self, name: &str) -> Result<EnvStatusSummary, String> {
        let env = self.get(name)?;
        let (gateway_port, gateway_port_source) = self.resolve_effective_gateway_port(&env)?;
        let mut summary = EnvStatusSummary {
            env_name: env.name.clone(),
            root: env.root.clone(),
            gateway_port: Some(gateway_port),
            gateway_port_source: Some(gateway_port_source.to_string()),
            default_runtime: env.default_runtime.clone(),
            default_launcher: env.default_launcher.clone(),
            resolved_kind: None,
            resolved_name: None,
            binary_path: None,
            command: None,
            run_dir: None,
            runtime_source_kind: None,
            runtime_release_version: None,
            runtime_release_channel: None,
            runtime_health: None,
            managed_service_state: None,
            openclaw_state: None,
            global_service_state: None,
            service_definition_drift: None,
            service_live_exec_unverified: None,
            service_orphaned_live: None,
            service_issue: None,
            issue: None,
        };

        if let Ok(service) = ServiceService::new(self.env, self.cwd).status(name) {
            summary.managed_service_state = Some(service_managed_state(&service));
            summary.openclaw_state = Some(if service.running {
                "running".to_string()
            } else if service.desired_running {
                "pending".to_string()
            } else {
                "stopped".to_string()
            });
            summary.global_service_state = None;
            summary.service_definition_drift = None;
            summary.service_live_exec_unverified = None;
            summary.service_orphaned_live = None;
            summary.service_issue = service.issue.clone();
        }

        if let Some(source) = self.active_source_watch_override(&env.name)? {
            summary.resolved_kind = Some("source-watch".to_string());
            summary.resolved_name = Some("source-watch".to_string());
            summary.command = Some(source.command_label());
            summary.binary_path = Some(source.openclaw_entry_path().display().to_string());
            summary.run_dir = Some(source.repo_root);
            return Ok(summary);
        }

        match resolve_execution_binding(&env, None, None) {
            Ok(ExecutionBinding::Runtime(runtime_name)) => {
                summary.resolved_kind = Some("runtime".to_string());
                summary.resolved_name = Some(runtime_name.clone());
                match crate::store::get_runtime(&runtime_name, self.env, self.cwd) {
                    Ok(runtime) => {
                        summary.binary_path = Some(runtime.binary_path.clone());
                        summary.run_dir =
                            Some(resolve_runtime_run_dir(self.cwd).display().to_string());
                        summary.runtime_source_kind =
                            Some(runtime.source_kind.as_str().to_string());
                        summary.runtime_release_version = runtime.release_version.clone();
                        summary.runtime_release_channel = runtime.release_channel.clone();
                        // Status checks whether the runtime can execute. Full-tree integrity remains
                        // an explicit `runtime verify` operation because packaged trees can be large.
                        match runtime_operational_issue(&runtime, self.env) {
                            None => summary.runtime_health = Some("ok".to_string()),
                            Some(error) => {
                                summary.runtime_health = Some("broken".to_string());
                                summary.issue =
                                    Some(format!("runtime \"{}\" {error}", runtime.name));
                            }
                        }
                    }
                    Err(error) => {
                        summary.runtime_health = Some("missing".to_string());
                        summary.issue = Some(error);
                    }
                }
            }
            Ok(ExecutionBinding::Launcher(launcher_name)) => {
                summary.resolved_kind = Some("launcher".to_string());
                summary.resolved_name = Some(launcher_name.clone());
                match get_launcher(&launcher_name, self.env, self.cwd) {
                    Ok(launcher) => {
                        summary.command = Some(launcher.command.clone());
                        summary.run_dir = Some(
                            resolve_launcher_run_dir(&launcher, self.cwd)
                                .display()
                                .to_string(),
                        );
                    }
                    Err(error) => summary.issue = Some(error),
                }
            }
            Ok(ExecutionBinding::Dev) => {
                summary.resolved_kind = Some("dev".to_string());
                summary.resolved_name = Some("dev".to_string());
                if let Some(dev) = env.dev.as_ref() {
                    summary.command = Some("pnpm openclaw".to_string());
                    summary.binary_path = Some("pnpm".to_string());
                    summary.run_dir = Some(dev.worktree_root.clone());
                } else {
                    summary.issue = Some(format!(
                        "environment \"{}\" is missing its dev binding",
                        env.name
                    ));
                }
            }
            Err(error) => summary.issue = Some(error),
        }

        Ok(summary)
    }
}

fn service_managed_state(summary: &crate::service::ServiceSummary) -> String {
    if summary.running {
        "running".to_string()
    } else if summary.loaded {
        "loaded".to_string()
    } else if summary.installed {
        "installed".to_string()
    } else {
        "absent".to_string()
    }
}
