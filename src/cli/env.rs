use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::fs;
use std::path::Path;
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::thread::sleep;
#[cfg(unix)]
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{Cli, render};
use crate::env::{
    CloneEnvironmentOptions, CreateEnvSnapshotOptions, CreateEnvironmentOptions, EnvMeta,
    EnvSnapshotSummary, EnvSummary, ExportEnvironmentOptions, ImportEnvironmentOptions,
    RemoveEnvSnapshotOptions, RestoreEnvSnapshotOptions,
};
use crate::infra::process::{run_direct, run_shell};
use crate::infra::shell::{
    build_openclaw_dev_source_env, build_openclaw_env, render_use_script, resolve_shell_name,
};
use crate::service::ServiceSummary;
use crate::store::{
    clear_skip_bootstrap_for_openclaw_onboarding, derive_env_paths, summarize_env, validate_name,
};

fn is_openclaw_root_option_value(value: &str) -> bool {
    if value.is_empty() || value == "--" {
        return false;
    }
    if !value.starts_with('-') {
        return true;
    }
    let Some(number) = value.strip_prefix('-') else {
        return false;
    };
    let mut parts = number.split('.');
    let Some(integer) = parts.next() else {
        return false;
    };
    !integer.is_empty()
        && integer.chars().all(|ch| ch.is_ascii_digit())
        && parts.next().is_none_or(|fraction| {
            !fraction.is_empty() && fraction.chars().all(|ch| ch.is_ascii_digit())
        })
        && parts.next().is_none()
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnvDestroyStepSummary {
    pub kind: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnvDestroyProcessIdentity {
    // Parent, cwd, and argv can change or contain credentials. PID plus a
    // normalized start time detects reuse without exposing mutable details.
    pid: u32,
    started_at: String,
}

fn managed_gateway_lifecycle_action(args: &[String]) -> Option<&str> {
    let mut command = None;
    let mut action = None;
    let mut index = 0;
    while index < args.len() && action.is_none() {
        let arg = args.get(index)?.as_str();
        if arg == "--" {
            break;
        }
        let consumed = match arg {
            "--dev" | "--no-color" => 1,
            "--profile" | "--log-level" | "--container" => {
                if args
                    .get(index + 1)
                    .is_some_and(|value| is_openclaw_root_option_value(value))
                {
                    2
                } else {
                    1
                }
            }
            _ if arg.starts_with("--profile=")
                || arg.starts_with("--log-level=")
                || arg.starts_with("--container=") =>
            {
                1
            }
            _ => 0,
        };
        if consumed > 0 {
            index += consumed;
            continue;
        }
        if !arg.starts_with('-') {
            if command.is_none() {
                command = Some(arg);
            } else {
                action = Some(arg);
            }
        }
        index += 1;
    }

    let command = command?;
    let action = action?;
    if !matches!(command, "gateway" | "daemon") {
        return None;
    }
    matches!(
        action,
        "install" | "uninstall" | "start" | "stop" | "restart"
    )
    .then_some(action)
}

#[derive(Debug)]
struct ProcessTerminationError {
    terminated: usize,
    message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnvDestroySummary {
    pub env_name: String,
    pub root: String,
    pub dev_worktree: Option<String>,
    pub protected: bool,
    pub apply: bool,
    pub force: bool,
    pub snapshot_count: usize,
    pub service_installed: bool,
    pub service_loaded: bool,
    pub service_running: bool,
    pub service_label: String,
    pub process_count: usize,
    #[serde(skip)]
    pub(crate) process_candidates: Vec<EnvDestroyProcessIdentity>,
    pub state_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub blockers: Vec<String>,
    pub steps: Vec<EnvDestroyStepSummary>,
    pub snapshots_removed: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub service_uninstalled: bool,
    pub processes_terminated: usize,
    pub worktree_removed: bool,
    pub removed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvDestroyState<'a> {
    kind: &'static str,
    environment: &'a EnvMeta,
    service: &'a ServiceSummary,
    process_candidates: &'a [EnvDestroyProcessIdentity],
    snapshots: &'a [EnvSnapshotSummary],
}

fn should_clear_skip_bootstrap_for_openclaw_args(args: &[String]) -> bool {
    if !matches!(args.first().map(String::as_str), Some("onboard" | "setup")) {
        return false;
    }

    !args.iter().any(|arg| arg == "--skip-bootstrap")
}

impl Cli {
    pub(super) fn handle_env_protect(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env protect")?;
        if args.len() < 2 {
            return Err(format!(
                "usage: {} env protect <env> <on|off>",
                self.command_example()
            ));
        }
        let name = &args[0];
        let value = args[1].trim().to_ascii_lowercase();
        Self::assert_no_extra_args(&args[2..])?;
        if value != "on" && value != "off" {
            return Err("protection must be \"on\" or \"off\"".to_string());
        }

        let meta = self
            .environment_service()
            .set_protected(name, value == "on")?;
        if json_flag {
            self.print_json(&meta)?;
            return Ok(0);
        }
        self.stdout_lines(render::env::env_protected(
            &meta.name,
            meta.protected,
            profile,
        ));
        Ok(0)
    }

    pub(super) fn handle_env_remove(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env remove")?;
        let (args, force) = Self::consume_flag(args, "--force");
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let meta = self.environment_service().remove(name, force)?;
        if json_flag {
            self.print_json(&meta)?;
            return Ok(0);
        }
        let root = derive_env_paths(Path::new(&meta.root))
            .root
            .display()
            .to_string();
        self.stdout_lines(render::env::env_removed(&meta.name, &root, profile));
        Ok(0)
    }

    pub(super) fn handle_env_destroy(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env destroy")?;
        let (args, yes) = Self::consume_flag(args, "--yes");
        let (args, force) = Self::consume_flag(args, "--force");
        let (args, expected_state_token) = Self::consume_option(args, "--if-state-token")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        if expected_state_token.is_some() && !yes {
            return Err("--if-state-token requires --yes".to_string());
        }
        if expected_state_token
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err("--if-state-token requires a non-empty value".to_string());
        }
        if expected_state_token.is_some() && force {
            return Err("env destroy accepts only one of --force or --if-state-token".to_string());
        }

        if !yes {
            let _operation_lock = self.environment_service().lock_operation(name)?;
            let summary = self.build_env_destroy_summary(name, false, force)?;
            if json_flag {
                self.print_json(&summary)?;
                return Ok(0);
            }

            self.stdout_lines(render::env::env_destroy_preview(
                &summary,
                profile,
                &self.command_example(),
            ));
            return Ok(0);
        }

        // Service and binding mutations use the same per-env lock. Keep it
        // through validation and teardown so a successful guard cannot go stale.
        let _operation_lock = self.environment_service().lock_operation(name)?;
        let mut summary = self.build_env_destroy_summary(name, true, force)?;
        if expected_state_token
            .as_deref()
            .is_some_and(|expected| expected != summary.state_token)
        {
            summary.code = Some("state_changed".to_string());
            summary.blockers.push(
                "environment state changed since destroy preview; request a new preview"
                    .to_string(),
            );
            if json_flag {
                self.print_json(&summary)?;
            } else {
                self.stdout_lines(render::env::env_destroy_preview(
                    &summary,
                    profile,
                    &self.command_example(),
                ));
            }
            return Ok(1);
        }

        if !summary.blockers.is_empty() {
            if json_flag {
                self.print_json(&summary)?;
            } else {
                self.stdout_lines(render::env::env_destroy_preview(
                    &summary,
                    profile,
                    &self.command_example(),
                ));
            }
            return Ok(1);
        }

        let env_meta = self.environment_service().get(name)?;

        let snapshot_ids = self
            .environment_service()
            .list_snapshots(Some(name))?
            .into_iter()
            .map(|snapshot| snapshot.id)
            .collect::<Vec<_>>();

        if summary.service_installed || summary.service_loaded || summary.service_running {
            self.service_service().uninstall_locked(name)?;
            summary.service_uninstalled = true;
        }

        if expected_state_token.is_some() {
            summary.processes_terminated =
                match self.terminate_env_processes_exact(&summary.process_candidates) {
                    Ok(count) => count,
                    Err(error) => {
                        summary.processes_terminated = error.terminated;
                        summary.code = Some("partial_apply".to_string());
                        summary.blockers.push(format!(
                            "process teardown failed after environment teardown began: {}",
                            error.message
                        ));
                        if json_flag {
                            self.print_json(&summary)?;
                        } else {
                            self.stdout_lines(render::env::env_destroy_preview(
                                &summary,
                                profile,
                                &self.command_example(),
                            ));
                        }
                        return Ok(1);
                    }
                };
        } else {
            summary.processes_terminated = self.terminate_env_processes(&env_meta)?;
        }
        let process_change = if expected_state_token.is_some() {
            match self.destroy_process_candidates(&env_meta) {
                Ok(candidates) if candidates.is_empty() => None,
                Ok(_) => Some(
                    "environment process state changed after teardown began; preview again to finish cleanup"
                        .to_string(),
                ),
                Err(error) => Some(format!(
                    "process state could not be verified after teardown began: {error}"
                )),
            }
        } else {
            None
        };
        if let Some(blocker) = process_change {
            summary.code = Some("partial_apply".to_string());
            summary.blockers.push(blocker);
            if json_flag {
                self.print_json(&summary)?;
            } else {
                self.stdout_lines(render::env::env_destroy_preview(
                    &summary,
                    profile,
                    &self.command_example(),
                ));
            }
            return Ok(1);
        }

        self.environment_service().remove_locked(name, force)?;
        summary.removed = true;
        summary.worktree_removed = env_meta
            .dev
            .as_ref()
            .is_some_and(|dev| !Path::new(&dev.worktree_root).exists());

        // Snapshots are the recovery path if destructive cleanup fails, so
        // remove them only after the environment and worktree are gone.
        for snapshot_id in &snapshot_ids {
            let removed =
                self.environment_service()
                    .remove_snapshot_locked(RemoveEnvSnapshotOptions {
                        env_name: name.clone(),
                        snapshot_id: snapshot_id.clone(),
                    })?;
            summary.warnings.extend(
                removed
                    .warnings
                    .into_iter()
                    .map(|warning| format!("snapshot {snapshot_id}: {warning}")),
            );
        }
        summary.snapshots_removed = snapshot_ids.len();

        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_destroyed(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_env_prune(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env prune")?;
        let (args, yes) = Self::consume_flag(args, "--yes");
        let (args, older_than_raw) = Self::consume_option(args, "--older-than")?;
        Self::assert_no_extra_args(&args)?;

        let older_than_days = match older_than_raw.as_deref() {
            Some(raw) => Self::parse_positive_u32(raw, "--older-than")? as i64,
            _ => 14,
        };

        let candidates = self
            .environment_service()
            .prune_candidates(older_than_days)?;
        let candidate_summaries = candidates.iter().map(summarize_env).collect::<Vec<_>>();

        if !yes {
            if json_flag {
                self.print_json(&serde_json::json!({
                    "apply": false,
                    "olderThanDays": older_than_days,
                    "count": candidate_summaries.len(),
                    "candidates": candidate_summaries,
                }))?;
                return Ok(0);
            }

            self.stdout_lines(render::env::env_prune_preview(
                older_than_days,
                &candidate_summaries,
                profile,
            ));
            return Ok(0);
        }

        let mut removed = Vec::<EnvSummary>::new();
        let removed_meta = self.environment_service().prune(older_than_days)?;
        for meta in removed_meta {
            removed.push(summarize_env(&meta));
        }

        if json_flag {
            self.print_json(&serde_json::json!({
                "apply": true,
                "olderThanDays": older_than_days,
                "count": removed.len(),
                "removed": removed,
            }))?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_pruned(&removed, profile));
        Ok(0)
    }

    pub(super) fn handle_env_create(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env create")?;
        let (args, protect) = Self::consume_flag(args, "--protect");
        let (args, root) = Self::consume_option(args, "--root")?;
        let (args, port_raw) = Self::consume_option(args, "--port")?;
        let gateway_port = match port_raw.as_deref() {
            Some(raw) => Some(Self::parse_positive_u32(raw, "--port")?),
            _ => None,
        };
        let (args, version) = Self::consume_option(args, "--version")?;
        let version = Self::require_option_value(version, "--version")?;
        let (args, channel) = Self::consume_option(args, "--channel")?;
        let channel = Self::require_option_value(channel, "--channel")?;
        let (args, runtime_name) = Self::consume_option(args, "--runtime")?;
        let runtime_name = Self::require_option_value(runtime_name, "--runtime")?;
        let (args, launcher_name) = Self::consume_option(args, "--launcher")?;
        let launcher_name = Self::require_option_value(launcher_name, "--launcher")?;

        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let uses_release_selector = version.is_some() || channel.is_some();
        let runtime_name = if uses_release_selector {
            self.with_progress(format!("Preparing OpenClaw runtime for {name}"), || {
                self.environment_service().resolve_runtime_binding_request(
                    runtime_name,
                    version,
                    channel,
                    "env create",
                )
            })?
        } else {
            self.environment_service().resolve_runtime_binding_request(
                runtime_name,
                version,
                channel,
                "env create",
            )?
        };
        let gateway_port_source = if gateway_port.is_some() {
            "metadata"
        } else {
            "computed"
        };
        let meta = self
            .environment_service()
            .create(CreateEnvironmentOptions {
                name: name.clone(),
                root,
                gateway_port,
                service_enabled: false,
                service_running: false,
                default_runtime: runtime_name,
                default_launcher: launcher_name,
                dev: None,
                protected: protect,
            })?;

        let meta = self
            .environment_service()
            .apply_effective_gateway_port(meta)?;
        if json_flag {
            self.print_json(&summarize_env(&meta))?;
            return Ok(0);
        }

        let summary = summarize_env(&meta);
        self.stdout_lines(render::env::env_created(
            &summary,
            Some(gateway_port_source),
            &self.command_example(),
            profile,
        ));
        Ok(0)
    }

    pub(super) fn handle_env_clone(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env clone")?;
        let (args, root) = Self::consume_option(args, "--root")?;
        let (args, sandbox_origin) = Self::consume_option(args, "--sandbox-origin")?;
        let sandbox_origin = Self::require_option_value(sandbox_origin, "--sandbox-origin")?;
        let Some(source_name) = args.first() else {
            return Err("source environment name is required".to_string());
        };
        let Some(target_name) = args.get(1) else {
            return Err("target environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[2..])?;

        let result = self.with_progress(
            format!("Cloning env {source_name} to {target_name}"),
            || {
                self.environment_service().clone_with_sandbox_origin(
                    CloneEnvironmentOptions {
                        source_name: source_name.clone(),
                        name: target_name.clone(),
                        root,
                    },
                    sandbox_origin,
                )
            },
        )?;
        let meta = result.meta;
        let (gateway_port, gateway_port_source) = self
            .environment_service()
            .resolve_effective_gateway_port(&meta)?;
        self.warn_cleared_sandbox_origin(
            &meta.name,
            result.cleared_sandbox_origin,
            result.sandbox_port,
        );

        if json_flag {
            self.print_json(&summarize_env(&meta))?;
            return Ok(0);
        }

        let mut display_meta = meta.clone();
        display_meta.gateway_port = Some(gateway_port);
        let summary = summarize_env(&display_meta);
        self.stdout_lines(render::env::env_cloned(
            &summary,
            Some(gateway_port_source),
            source_name,
            &self.command_example(),
            profile,
        ));
        Ok(0)
    }

    pub(super) fn handle_env_export(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env export")?;
        let (args, output) = Self::consume_option(args, "--output")?;
        let output = Self::require_option_value(output, "--output")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary = self.with_progress(format!("Exporting env {name}"), || {
            self.environment_service().export(ExportEnvironmentOptions {
                name: name.clone(),
                output,
            })
        })?;

        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_exported(&summary, profile));
        Ok(0)
    }

    pub(super) fn handle_env_import(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env import")?;
        let (args, name) = Self::consume_option(args, "--name")?;
        let name = Self::require_option_value(name, "--name")?;
        let (args, root) = Self::consume_option(args, "--root")?;
        let root = Self::require_option_value(root, "--root")?;
        let (args, sandbox_origin) = Self::consume_option(args, "--sandbox-origin")?;
        let sandbox_origin = Self::require_option_value(sandbox_origin, "--sandbox-origin")?;
        let Some(archive) = args.first() else {
            return Err("archive path is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let result = self.with_progress("Importing environment archive", || {
            self.environment_service().import_with_sandbox_origin(
                ImportEnvironmentOptions {
                    archive: archive.clone(),
                    name,
                    root,
                },
                sandbox_origin,
            )
        })?;
        let summary = result.summary;
        self.warn_cleared_sandbox_origin(
            &summary.name,
            result.cleared_sandbox_origin,
            result.sandbox_port,
        );

        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_imported(
            &summary,
            &self.command_example(),
            profile,
        ));
        Ok(0)
    }

    pub(super) fn handle_env_doctor(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env doctor")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let doctor = self.environment_service().doctor(name)?;
        if json_flag {
            self.print_json(&doctor)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_doctor(
            &doctor,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_env_cleanup(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env cleanup")?;
        let (args, yes_flag) = Self::consume_flag(args, "--yes");
        let (args, all_flag) = Self::consume_flag(args, "--all");

        if all_flag {
            if !args.is_empty() {
                return Err("env cleanup accepts either <name> or --all".to_string());
            }
            let cleanup = if yes_flag {
                self.environment_service().cleanup_all()?
            } else {
                self.environment_service().cleanup_all_preview()?
            };
            if json_flag {
                self.print_json(&cleanup)?;
                return Ok(0);
            }

            self.stdout_lines(render::env::env_cleanup_batch(&cleanup, profile));
            return Ok(0);
        }

        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let cleanup = if yes_flag {
            self.environment_service().cleanup(name)?
        } else {
            self.environment_service().cleanup_preview(name)?
        };
        if json_flag {
            self.print_json(&cleanup)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_cleanup(&cleanup, profile));
        Ok(0)
    }

    pub(super) fn handle_env_list(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env list")?;
        Self::assert_no_extra_args(&args)?;

        let envs = self.environment_service().list()?;
        let summaries = envs
            .into_iter()
            .map(|meta| {
                self.environment_service()
                    .apply_effective_gateway_port(meta)
            })
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .map(summarize_env)
            .collect::<Vec<_>>();
        if json_flag {
            self.print_json(&summaries)?;
            return Ok(0);
        }
        self.stdout_lines(render::env::env_list(&summaries, profile));
        Ok(0)
    }

    pub(super) fn handle_env_show(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env show")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let meta = self
            .environment_service()
            .apply_effective_gateway_port(self.environment_service().get(name)?)?;
        let summary = summarize_env(&meta);
        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_show(
            &summary,
            profile,
            &self.command_example(),
        )?);
        Ok(0)
    }

    pub(super) fn handle_env_status(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "env status")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let status = self.environment_service().status(name)?;
        if json_flag {
            self.print_json(&status)?;
            return Ok(0);
        }
        self.stdout_lines(render::env::env_status(
            &status,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    fn handle_env_snapshot_create(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "env snapshot create")?;
        let (args, label) = Self::consume_option(args, "--label")?;
        let label = Self::require_option_value(label, "--label")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let snapshot = self.with_progress(format!("Creating snapshot for {name}"), || {
            let _operation_lock = self.environment_service().lock_operation(name)?;
            let prepared = self
                .environment_service()
                .prepare_snapshot_capture_locked(name)?;
            let service_state = self
                .service_service()
                .quiesce_for_snapshot_locked(name)?;
            let snapshot_result = self
                .environment_service()
                .create_snapshot_locked_from_preparation(
                    CreateEnvSnapshotOptions {
                        env_name: name.clone(),
                        label,
                    },
                    service_state.map(|state| (state.enabled, state.running)),
                    prepared,
                );
            let service_result = self
                .service_service()
                .restore_after_snapshot_locked(name, service_state);
            match (snapshot_result, service_result) {
                (Ok(snapshot), Ok(())) => Ok(snapshot),
                (Ok(snapshot), Err(service_error)) => Err(format!(
                    "snapshot {} was created, but the managed service could not be restored: {service_error}",
                    snapshot.id
                )),
                (Err(snapshot_error), Ok(())) => Err(snapshot_error),
                (Err(snapshot_error), Err(service_error)) => Err(format!(
                    "{snapshot_error}; also failed to restore the managed service after snapshot capture: {service_error}"
                )),
            }
        })?;

        if json_flag {
            self.print_json(&snapshot)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_snapshot_created(&snapshot, profile));
        Ok(0)
    }

    fn handle_env_snapshot_show(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "env snapshot show")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        let Some(snapshot_id) = args.get(1) else {
            return Err("snapshot id is required".to_string());
        };
        Self::assert_no_extra_args(&args[2..])?;

        let snapshot = self.environment_service().get_snapshot(name, snapshot_id)?;
        if json_flag {
            self.print_json(&snapshot)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_snapshot_show(&snapshot, profile)?);
        Ok(0)
    }

    fn handle_env_snapshot_list(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "env snapshot list")?;
        let (args, all) = Self::consume_flag(args, "--all");
        let env_name = if all {
            if !args.is_empty() {
                return Err("env snapshot list accepts either <name> or --all".to_string());
            }
            None
        } else {
            let Some(name) = args.first() else {
                return Err("environment name is required".to_string());
            };
            Self::assert_no_extra_args(&args[1..])?;
            Some(name.as_str())
        };

        let snapshots = self.environment_service().list_snapshots(env_name)?;
        if json_flag {
            self.print_json(&snapshots)?;
            return Ok(0);
        }
        self.stdout_lines(render::env::env_snapshot_list(&snapshots, profile)?);
        Ok(0)
    }

    fn handle_env_snapshot_restore(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "env snapshot restore")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        let Some(snapshot_id) = args.get(1) else {
            return Err("snapshot id is required".to_string());
        };
        Self::assert_no_extra_args(&args[2..])?;

        let restored = self.with_progress(
            format!("Restoring snapshot {snapshot_id} for {name}"),
            || {
                let _operation_lock = self.environment_service().lock_operation(name)?;
                let selected_snapshot = self
                    .environment_service()
                    .get_snapshot(name, snapshot_id)?;
                let service_state = self
                    .service_service()
                    .quiesce_for_snapshot_locked(name)?;
                let restore_result = self.environment_service().prepare_snapshot_restore_locked(
                    RestoreEnvSnapshotOptions {
                        env_name: name.clone(),
                        snapshot_id: snapshot_id.clone(),
                    },
                );
                match restore_result {
                    Ok(transaction) => {
                        let restored = transaction.summary.clone();
                        let restored_service_state =
                            if selected_snapshot.service_enabled && selected_snapshot.service_running {
                                Some(crate::service::ServiceMaintenanceState {
                                    enabled: true,
                                    running: true,
                                })
                            } else {
                                None
                            };
                        let service_acceptance = self
                            .service_service()
                            .restore_after_snapshot_locked(name, restored_service_state)
                            .and_then(|()| {
                                if restored_service_state.is_none() {
                                    return Ok(());
                                }
                                self.wait_for_restarted_gateway_health(name, true)
                            });
                        match service_acceptance {
                            Ok(()) => {
                                self.environment_service()
                                    .commit_snapshot_restore_locked(transaction)?;
                                Ok(restored)
                            }
                            Err(service_error) => {
                                let rollback_result = self
                                    .environment_service()
                                    .rollback_snapshot_restore_locked(transaction);
                                let service_restore_result = self
                                    .service_service()
                                    .restore_after_snapshot_locked(name, service_state);
                                let mut error = format!(
                                    "snapshot {} failed managed-service acceptance: {service_error}",
                                    restored.snapshot_id
                                );
                                if let Err(rollback_error) = rollback_result {
                                    error.push_str(&format!(
                                        "; restoring the displaced environment root also failed: {rollback_error}"
                                    ));
                                }
                                if let Err(restore_error) = service_restore_result {
                                    error.push_str(&format!(
                                        "; restoring the pre-restore managed service also failed: {restore_error}"
                                    ));
                                }
                                Err(error)
                            }
                        }
                    }
                    Err(restore_error) => match self
                        .service_service()
                        .restore_after_snapshot_locked(name, service_state)
                    {
                        Ok(()) => Err(restore_error),
                        Err(service_error) => Err(format!(
                            "{restore_error}; also failed to restore the pre-restore managed service state: {service_error}"
                        )),
                    },
                }
            },
        )?;

        if json_flag {
            self.print_json(&restored)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_snapshot_restored(&restored, profile));
        Ok(0)
    }

    fn handle_env_snapshot_remove(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "env snapshot remove")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        let Some(snapshot_id) = args.get(1) else {
            return Err("snapshot id is required".to_string());
        };
        Self::assert_no_extra_args(&args[2..])?;

        let removed = self
            .environment_service()
            .remove_snapshot(RemoveEnvSnapshotOptions {
                env_name: name.clone(),
                snapshot_id: snapshot_id.clone(),
            })?;

        if json_flag {
            self.print_json(&removed)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_snapshot_removed(&removed, profile));
        Ok(0)
    }

    fn handle_env_snapshot_prune(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "env snapshot prune")?;
        let (args, yes) = Self::consume_flag(args, "--yes");
        let (args, all) = Self::consume_flag(args, "--all");
        let (args, keep_raw) = Self::consume_option(args, "--keep")?;
        let keep = match keep_raw.as_deref() {
            Some(raw) => Some(Self::parse_positive_u32(raw, "--keep")? as usize),
            None => None,
        };
        let (args, older_than_raw) = Self::consume_option(args, "--older-than")?;
        let older_than_days = match older_than_raw.as_deref() {
            Some(raw) => Some(Self::parse_positive_u32(raw, "--older-than")? as i64),
            None => None,
        };

        let env_name = if all {
            if !args.is_empty() {
                return Err("env snapshot prune accepts either <name> or --all".to_string());
            }
            None
        } else {
            let Some(name) = args.first() else {
                return Err("environment name is required".to_string());
            };
            Self::assert_no_extra_args(&args[1..])?;
            Some(name.as_str())
        };

        if keep.is_none() && older_than_days.is_none() {
            return Err("env snapshot prune requires --keep or --older-than".to_string());
        }

        let scope_label = env_name.unwrap_or("all");
        if !yes {
            let candidates = self.environment_service().prune_snapshot_candidates(
                env_name,
                keep,
                older_than_days,
            )?;
            if json_flag {
                self.print_json(&serde_json::json!({
                    "apply": false,
                    "scope": scope_label,
                    "keep": keep,
                    "olderThanDays": older_than_days,
                    "count": candidates.len(),
                    "candidates": candidates,
                }))?;
                return Ok(0);
            }

            self.stdout_lines(render::env::env_snapshot_prune_preview(
                scope_label,
                &candidates,
                profile,
            )?);
            return Ok(0);
        }

        let removed =
            self.environment_service()
                .prune_snapshots(env_name, keep, older_than_days)?;

        if json_flag {
            self.print_json(&serde_json::json!({
                "apply": true,
                "scope": scope_label,
                "keep": keep,
                "olderThanDays": older_than_days,
                "count": removed.len(),
                "removed": removed,
            }))?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_snapshot_pruned(&removed, profile));
        Ok(0)
    }

    pub(super) fn dispatch_env_snapshot_command(&self, args: Vec<String>) -> Result<i32, String> {
        let Some(action) = args.first() else {
            return Err("env snapshot command is required".to_string());
        };

        match action.as_str() {
            "create" => self.handle_env_snapshot_create(args[1..].to_vec()),
            "show" => self.handle_env_snapshot_show(args[1..].to_vec()),
            "list" => self.handle_env_snapshot_list(args[1..].to_vec()),
            "restore" => self.handle_env_snapshot_restore(args[1..].to_vec()),
            "remove" => self.handle_env_snapshot_remove(args[1..].to_vec()),
            "prune" => self.handle_env_snapshot_prune(args[1..].to_vec()),
            _ => Err(format!("unknown env snapshot command: {action}")),
        }
    }

    pub(super) fn dispatch_env_command(
        &self,
        action: &str,
        args: Vec<String>,
    ) -> Result<i32, String> {
        match action {
            "create" => self.handle_env_create(args),
            "clone" => self.handle_env_clone(args),
            "export" => self.handle_env_export(args),
            "import" => self.handle_env_import(args),
            "snapshot" => self.dispatch_env_snapshot_command(args),
            "list" => self.handle_env_list(args),
            "show" => self.handle_env_show(args),
            "status" => self.handle_env_status(args),
            "doctor" => self.handle_env_doctor(args),
            "cleanup" => self.handle_env_cleanup(args),
            "use" => self.handle_env_use(args),
            "exec" => self.handle_env_exec(args),
            "resolve" => self.handle_env_resolve(args),
            "run" => self.handle_env_run(args),
            "set-runtime" => self.handle_env_set_runtime(args),
            "set-launcher" => self.handle_env_set_launcher(args),
            "protect" => self.handle_env_protect(args),
            "destroy" => self.handle_env_destroy(args),
            "remove" | "rm" => self.handle_env_remove(args),
            "prune" => self.handle_env_prune(args),
            _ => Err(format!("unknown env command: {action}")),
        }
    }
}

impl Cli {
    fn build_env_destroy_summary(
        &self,
        name: &str,
        apply: bool,
        force: bool,
    ) -> Result<EnvDestroySummary, String> {
        let env_meta = self.environment_service().get(name)?;
        let service = self.service_service().status(name)?;
        let mut snapshots = self.environment_service().list_snapshots(Some(name))?;
        snapshots.sort_by(|left, right| left.id.cmp(&right.id));
        let mut blockers = Vec::new();
        let process_candidates = self.destroy_process_candidates(&env_meta)?;
        let state_token =
            env_destroy_state_token(&env_meta, &service, &process_candidates, &snapshots)?;

        if env_meta.protected && !force {
            blockers.push("env is protected; re-run with --force to destroy it".to_string());
        }
        let mut steps = Vec::new();
        if service.installed || service.loaded || service.running {
            steps.push(EnvDestroyStepSummary {
                kind: "service".to_string(),
                description: "disable env gateway in the OCM background service".to_string(),
            });
        }
        if !process_candidates.is_empty() {
            steps.push(EnvDestroyStepSummary {
                kind: "processes".to_string(),
                description: "terminate live OpenClaw processes for the env".to_string(),
            });
        }
        if let Some(dev) = env_meta.dev.as_ref() {
            steps.push(EnvDestroyStepSummary {
                kind: "worktree".to_string(),
                description: format!("remove dev worktree {}", dev.worktree_root),
            });
        }
        steps.push(EnvDestroyStepSummary {
            kind: "env".to_string(),
            description: "remove env root and metadata".to_string(),
        });
        if !snapshots.is_empty() {
            steps.push(EnvDestroyStepSummary {
                kind: "snapshots".to_string(),
                description: format!("remove {} env snapshot(s)", snapshots.len()),
            });
        }

        Ok(EnvDestroySummary {
            env_name: env_meta.name,
            root: env_meta.root,
            dev_worktree: env_meta.dev.as_ref().map(|dev| dev.worktree_root.clone()),
            protected: env_meta.protected,
            apply,
            force,
            snapshot_count: snapshots.len(),
            service_installed: service.installed,
            service_loaded: service.loaded,
            service_running: service.running,
            service_label: "ocm".to_string(),
            process_count: process_candidates.len(),
            process_candidates,
            state_token,
            code: None,
            blockers,
            steps,
            snapshots_removed: 0,
            warnings: Vec::new(),
            service_uninstalled: false,
            processes_terminated: 0,
            worktree_removed: false,
            removed: false,
        })
    }

    fn terminate_env_processes(&self, meta: &EnvMeta) -> Result<usize, String> {
        terminate_associated_processes(meta)
    }

    fn terminate_env_processes_exact(
        &self,
        candidates: &[EnvDestroyProcessIdentity],
    ) -> Result<usize, ProcessTerminationError> {
        terminate_exact_processes(candidates)
    }

    fn destroy_process_candidates(
        &self,
        meta: &EnvMeta,
    ) -> Result<Vec<EnvDestroyProcessIdentity>, String> {
        associated_processes(meta)
    }

    pub(super) fn handle_env_use(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, shell_name) = Self::consume_option(args, "--shell")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let meta = self
            .environment_service()
            .apply_effective_gateway_port(self.environment_service().touch(name)?)?;
        let shell = resolve_shell_name(shell_name.as_deref(), &self.env);
        self.stdout_text(&render_use_script(&meta, &shell, &self.env))?;
        Ok(0)
    }

    pub(super) fn handle_env_exec(&self, args: Vec<String>) -> Result<i32, String> {
        let (before, after) = Self::split_on_double_dash(&args);
        let Some(name) = before.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_command_separator(&before, "env exec requires -- before the command")?;
        if after.is_empty() {
            return Err("env exec requires a command after --".to_string());
        }

        let meta = self
            .environment_service()
            .apply_effective_gateway_port(self.environment_service().touch(name)?)?;
        if meta.service_enabled
            && meta.service_running
            && after.first().is_some_and(|command| command == "openclaw")
            && managed_gateway_lifecycle_action(&after[1..]).is_some()
        {
            let command = self.command_example();
            return Err(format!(
                "env exec cannot safely run OpenClaw gateway lifecycle commands for supervised env \"{}\"; use \"{command} @{} -- gateway restart\" or \"{command} service restart {}\"",
                meta.name, meta.name, meta.name
            ));
        }
        if let Some(source) = self
            .environment_service()
            .active_source_watch_override(&meta.name)?
        {
            let source_env =
                build_openclaw_dev_source_env(&meta, &self.env, Path::new(&source.repo_root));
            if after[0] == "openclaw" {
                let mut node_args = vec![source.openclaw_entry_path().display().to_string()];
                node_args.extend(after[1..].iter().cloned());
                return run_direct(
                    "node",
                    &node_args,
                    &source_env,
                    Path::new(&source.repo_root),
                );
            }
            return run_direct(&after[0], &after[1..], &source_env, &self.cwd);
        }
        run_direct(
            &after[0],
            &after[1..],
            &build_openclaw_env(&meta, &self.env),
            &self.cwd,
        )
    }

    pub(super) fn handle_env_resolve(&self, args: Vec<String>) -> Result<i32, String> {
        let (before, after) = Self::split_on_double_dash(&args);
        let (before, json_flag, profile) =
            self.consume_human_output_flags(before, "env resolve")?;
        let (before, runtime_override) = Self::consume_option(before, "--runtime")?;
        let runtime_override = Self::require_option_value(runtime_override, "--runtime")?;
        let (before, launcher_override) = Self::consume_option(before, "--launcher")?;
        let launcher_override = Self::require_option_value(launcher_override, "--launcher")?;
        let Some(name) = before.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&before[1..])?;

        let summary = self
            .environment_service()
            .resolve(name, runtime_override, launcher_override, &after)?
            .into_summary();

        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::env::env_resolved(&summary, profile));
        Ok(0)
    }

    pub(super) fn handle_env_run(&self, args: Vec<String>) -> Result<i32, String> {
        let (before, after) = Self::split_on_double_dash(&args);
        let (before, runtime_override) = Self::consume_option(before, "--runtime")?;
        let runtime_override = Self::require_option_value(runtime_override, "--runtime")?;
        let (before, launcher_override) = Self::consume_option(before, "--launcher")?;
        let launcher_override = Self::require_option_value(launcher_override, "--launcher")?;
        let Some(name) = before.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_command_separator(&before, "env run requires -- before OpenClaw arguments")?;

        let meta = self.environment_service().get(name)?;
        if meta.service_enabled
            && meta.service_running
            && let Some(action) = managed_gateway_lifecycle_action(&after)
        {
            let command = self.command_example();
            if action != "restart" {
                return Err(format!(
                    "OpenClaw cannot {action} the gateway service for supervised env \"{}\"; use \"{command} service {action} {}\"",
                    meta.name, meta.name
                ));
            }
            if runtime_override.is_some() || launcher_override.is_some() {
                return Err(format!(
                    "gateway restart for supervised env \"{}\" must use its active binding; use \"{command} service restart {}\"",
                    meta.name, meta.name
                ));
            }
            let inspection =
                crate::supervisor::SupervisorService::new(&self.env, &self.cwd).inspect()?;
            let restart_handoff = inspection
                .runtime_services
                .iter()
                .find(|service| service.env_name == meta.name)
                .and_then(|service| service.restart_handoff.as_deref());
            if restart_handoff != Some("protocol-v1") {
                return Err(format!(
                    "env \"{}\" has not negotiated external restart handoff protocol v1; use \"{command} service restart {}\" or upgrade its OpenClaw runtime",
                    meta.name, meta.name
                ));
            }
        }

        if should_clear_skip_bootstrap_for_openclaw_args(&after) {
            clear_skip_bootstrap_for_openclaw_onboarding(&derive_env_paths(Path::new(&meta.root)))?;
        }

        let resolved = self.environment_service().resolve_run(
            name,
            runtime_override,
            launcher_override,
            &after,
        )?;
        match resolved {
            crate::env::ResolvedExecution::Launcher {
                env,
                command,
                run_dir,
                ..
            } => run_shell(&command, &build_openclaw_env(&env, &self.env), &run_dir),
            crate::env::ResolvedExecution::Runtime {
                env,
                program,
                program_args,
                path_prepend,
                run_dir,
                ..
            } => {
                let mut process_env = build_openclaw_env(&env, &self.env);
                crate::managed_node::apply_path_prepend_to_environment(
                    &mut process_env,
                    path_prepend.as_deref(),
                )?;
                run_direct(&program, &program_args, &process_env, &run_dir)
            }
            crate::env::ResolvedExecution::Dev {
                env,
                worktree_root,
                program,
                program_args,
                run_dir,
                ..
            } => run_direct(
                &program,
                &program_args,
                &build_openclaw_dev_source_env(&env, &self.env, Path::new(&worktree_root)),
                &run_dir,
            ),
            crate::env::ResolvedExecution::SourceWatch {
                env,
                source,
                program,
                program_args,
                run_dir,
                ..
            } => run_direct(
                &program,
                &program_args,
                &build_openclaw_dev_source_env(&env, &self.env, Path::new(&source.repo_root)),
                &run_dir,
            ),
        }
    }

    pub(super) fn handle_env_set_runtime(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "env set-runtime")?;
        let (args, version) = Self::consume_option(args, "--version")?;
        let version = Self::require_option_value(version, "--version")?;
        let (args, channel) = Self::consume_option(args, "--channel")?;
        let channel = Self::require_option_value(channel, "--channel")?;

        if args.is_empty() {
            return Err(format!(
                "usage: {} env set-runtime <env> <runtime|none>\n       {} env set-runtime <env> (--version <version> | --channel <channel>)",
                self.command_example(),
                self.command_example()
            ));
        }
        let name = &args[0];
        let runtime_name = args.get(1).cloned();
        let extra_args = if runtime_name.is_some() {
            &args[2..]
        } else {
            &args[1..]
        };
        Self::assert_no_extra_args(extra_args)?;
        if runtime_name.is_some() && (version.is_some() || channel.is_some()) {
            return Err(
                "env set-runtime accepts only one runtime source: --runtime, --version, or --channel"
                    .to_string(),
            );
        }
        if version.is_some() && channel.is_some() {
            return Err("env set-runtime accepts only one of --version or --channel".to_string());
        }

        let current = self.environment_service().get(name)?;
        let has_existing_binding =
            current.default_runtime.is_some() || current.default_launcher.is_some();
        let clears_runtime = matches!(runtime_name.as_deref(), Some(runtime) if runtime.eq_ignore_ascii_case("none"));
        if has_existing_binding && !clears_runtime {
            let runtime = runtime_name
                .map(|runtime| validate_name(&runtime, "Runtime name"))
                .transpose()?;
            let summary = self.upgrade_env_to_runtime_target(name, version, channel, runtime)?;
            if matches!(
                summary.outcome.as_str(),
                "failed" | "rolled-back" | "rollback-failed"
            ) {
                return Err(summary.note.unwrap_or_else(|| {
                    format!("runtime transition failed with outcome {}", summary.outcome)
                }));
            }
            let meta = self.environment_service().get(name)?;
            if json_flag {
                self.print_json(&meta)?;
                return Ok(0);
            }
            let default_runtime = meta.default_runtime.unwrap_or_else(|| "none".to_string());
            self.stdout_lines(render::env::env_runtime_updated(
                &meta.name,
                &default_runtime,
                profile,
            ));
            return Ok(0);
        }

        let validated = match runtime_name {
            Some(runtime_name) if runtime_name.eq_ignore_ascii_case("none") => runtime_name,
            Some(runtime_name) => self
                .environment_service()
                .resolve_runtime_binding_request(
                    Some(validate_name(&runtime_name, "Runtime name")?),
                    version,
                    channel,
                    "env set-runtime",
                )?
                .unwrap_or(runtime_name),
            None if version.is_some() || channel.is_some() => self
                .with_progress(format!("Preparing OpenClaw runtime for {name}"), || {
                    self.environment_service().resolve_runtime_binding_request(
                        None,
                        version,
                        channel,
                        "env set-runtime",
                    )
                })?
                .ok_or_else(|| {
                    "env set-runtime requires a runtime, none, --version, or --channel".to_string()
                })?,
            None => {
                return Err(format!(
                    "usage: {} env set-runtime <env> <runtime|none>\n       {} env set-runtime <env> (--version <version> | --channel <channel>)",
                    self.command_example(),
                    self.command_example()
                ));
            }
        };
        let meta = self.environment_service().set_runtime(name, &validated)?;
        if json_flag {
            self.print_json(&meta)?;
            return Ok(0);
        }
        let default_runtime = meta.default_runtime.unwrap_or_else(|| "none".to_string());
        self.stdout_lines(render::env::env_runtime_updated(
            &meta.name,
            &default_runtime,
            profile,
        ));
        Ok(0)
    }

    pub(super) fn handle_env_set_launcher(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "env set-launcher")?;
        if args.len() < 2 {
            return Err(format!(
                "usage: {} env set-launcher <env> <launcher|none>",
                self.command_example()
            ));
        }
        let name = &args[0];
        let launcher_name = &args[1];
        Self::assert_no_extra_args(&args[2..])?;

        let validated = if launcher_name.eq_ignore_ascii_case("none") {
            launcher_name.to_string()
        } else {
            validate_name(launcher_name, "Launcher name")?
        };
        let meta = self.environment_service().set_launcher(name, &validated)?;
        if json_flag {
            self.print_json(&meta)?;
            return Ok(0);
        }
        let default_launcher = meta.default_launcher.unwrap_or_else(|| "none".to_string());
        self.stdout_lines(render::env::env_launcher_updated(
            &meta.name,
            &default_launcher,
            profile,
        ));
        Ok(0)
    }
}

fn env_destroy_state_token(
    environment: &EnvMeta,
    service: &ServiceSummary,
    process_candidates: &[EnvDestroyProcessIdentity],
    snapshots: &[EnvSnapshotSummary],
) -> Result<String, String> {
    let state = EnvDestroyState {
        kind: "ocm-env-destroy-state-v1",
        environment,
        service,
        process_candidates,
        snapshots,
    };
    let encoded = serde_json::to_vec(&state)
        .map_err(|error| format!("failed to encode environment destroy state: {error}"))?;
    let digest: String = Sha256::digest(encoded)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("v1:{digest}"))
}

#[cfg(unix)]
fn terminate_associated_processes(meta: &EnvMeta) -> Result<usize, String> {
    let candidates = associated_processes(meta)?;
    if candidates.is_empty() {
        return Ok(0);
    }

    terminate_exact_processes(&candidates).map_err(|error| error.message)?;

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if associated_processes(meta)?.is_empty() {
            return Ok(candidates.len());
        }
        sleep(Duration::from_millis(100));
    }

    Err("failed to terminate all live env processes".to_string())
}

#[cfg(not(unix))]
fn terminate_associated_processes(_meta: &EnvMeta) -> Result<usize, String> {
    Ok(0)
}

#[cfg(unix)]
fn terminate_exact_processes(
    candidates: &[EnvDestroyProcessIdentity],
) -> Result<usize, ProcessTerminationError> {
    let mut terminated = 0;
    for candidate in candidates {
        let current =
            current_process_identity(candidate.pid).map_err(|message| ProcessTerminationError {
                terminated,
                message,
            })?;
        if current.as_ref() != Some(candidate) {
            continue;
        }
        terminate_pid(candidate.pid).map_err(|message| ProcessTerminationError {
            terminated,
            message,
        })?;
        let current =
            current_process_identity(candidate.pid).map_err(|message| ProcessTerminationError {
                terminated,
                message,
            })?;
        if current.as_ref() == Some(candidate) {
            return Err(ProcessTerminationError {
                terminated,
                message: format!("failed to terminate pid {}", candidate.pid),
            });
        }
        terminated += 1;
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let mut remaining = false;
        for candidate in candidates {
            let current = current_process_identity(candidate.pid).map_err(|message| {
                ProcessTerminationError {
                    terminated,
                    message,
                }
            })?;
            if current.as_ref() == Some(candidate) {
                remaining = true;
                break;
            }
        }
        if !remaining {
            return Ok(terminated);
        }
        sleep(Duration::from_millis(100));
    }
    Err(ProcessTerminationError {
        terminated,
        message: "failed to terminate the previewed env processes".to_string(),
    })
}

#[cfg(not(unix))]
fn terminate_exact_processes(
    _candidates: &[EnvDestroyProcessIdentity],
) -> Result<usize, ProcessTerminationError> {
    Ok(0)
}

#[cfg(unix)]
fn associated_processes(meta: &EnvMeta) -> Result<Vec<EnvDestroyProcessIdentity>, String> {
    let first = associated_processes_once(meta)?;
    if first.is_empty() {
        return Ok(first);
    }
    let second = associated_processes_once(meta)?;
    if first != second {
        return Err(
            "environment process state changed during inspection; retry the command".to_string(),
        );
    }
    Ok(second)
}

#[cfg(unix)]
fn active_invocation_processes(
    current_pid: u32,
    parent_map: &std::collections::HashMap<u32, u32>,
    children_map: &std::collections::HashMap<u32, Vec<u32>>,
) -> BTreeSet<u32> {
    // The destroy command and its caller/inspection tree can mention the target
    // path without being owned by that environment.
    let mut excluded = BTreeSet::new();
    let mut cursor = current_pid;
    while cursor > 1 && excluded.insert(cursor) {
        let Some(parent) = parent_map.get(&cursor).copied() else {
            break;
        };
        if parent == cursor {
            break;
        }
        cursor = parent;
    }

    let mut queue = vec![current_pid];
    while let Some(pid) = queue.pop() {
        let Some(children) = children_map.get(&pid) else {
            continue;
        };
        for child in children {
            if excluded.insert(*child) {
                queue.push(*child);
            }
        }
    }
    excluded
}

#[cfg(unix)]
fn associated_processes_once(meta: &EnvMeta) -> Result<Vec<EnvDestroyProcessIdentity>, String> {
    let processes = process_table()?;
    let cwd_map = process_cwd_map(&processes)?;
    let parent_map = processes
        .iter()
        .map(|process| (process.pid, process.ppid))
        .collect::<std::collections::HashMap<_, _>>();
    let mut children_map = std::collections::HashMap::<u32, Vec<u32>>::new();
    for process in &processes {
        children_map
            .entry(process.ppid)
            .or_default()
            .push(process.pid);
    }
    let invocation_processes =
        active_invocation_processes(std::process::id(), &parent_map, &children_map);

    let mut seeds = BTreeSet::new();
    for process in &processes {
        if invocation_processes.contains(&process.pid)
            || interactive_shell_command(&process.command)
        {
            continue;
        }
        if process_belongs_to_env(process.pid, &process.command, meta, &cwd_map) {
            seeds.insert(process.pid);
        }
    }

    if seeds.is_empty() {
        return Ok(Vec::new());
    }

    let mut related = seeds;
    let mut queue = related.iter().copied().collect::<Vec<_>>();
    while let Some(pid) = queue.pop() {
        if let Some(children) = children_map.get(&pid) {
            for child in children {
                let Some(process) = processes.iter().find(|process| process.pid == *child) else {
                    continue;
                };
                if invocation_processes.contains(child)
                    || interactive_shell_command(&process.command)
                {
                    continue;
                }
                if related.insert(*child) {
                    queue.push(*child);
                }
            }
        }
    }

    let depths = process_depths(&parent_map);
    let mut ordered = related.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|pid| {
        (
            std::cmp::Reverse(depths.get(pid).copied().unwrap_or(0)),
            *pid,
        )
    });
    ordered
        .into_iter()
        .map(|pid| {
            process_start_id(pid)?
                .map(|started_at| EnvDestroyProcessIdentity { pid, started_at })
                .ok_or_else(|| {
                    "environment process state changed during inspection; retry the command"
                        .to_string()
                })
        })
        .collect()
}

#[cfg(not(unix))]
fn associated_processes(_meta: &EnvMeta) -> Result<Vec<EnvDestroyProcessIdentity>, String> {
    Ok(Vec::new())
}

#[cfg(unix)]
fn process_belongs_to_env(
    pid: u32,
    command: &str,
    meta: &EnvMeta,
    cwd_map: &std::collections::HashMap<u32, String>,
) -> bool {
    let paths = derive_env_paths(Path::new(&meta.root));
    let mut markers = vec![
        meta.root.clone(),
        paths.state_dir.to_string_lossy().into_owned(),
        paths.config_path.to_string_lossy().into_owned(),
        paths.workspace_dir.to_string_lossy().into_owned(),
    ]
    .into_iter()
    .map(|value| normalize_process_path(&value))
    .collect::<Vec<_>>();
    if let Some(dev) = meta.dev.as_ref() {
        markers.push(normalize_process_path(&dev.worktree_root));
    }

    let command = normalize_process_path(command);
    if markers
        .iter()
        .any(|marker| command_contains_process_path(&command, marker))
    {
        return true;
    }

    if let Some(cwd) = cwd_map.get(&pid)
        && markers
            .iter()
            .any(|marker| process_path_is_within(&normalize_process_path(cwd), marker))
    {
        return true;
    }

    false
}

#[cfg(unix)]
fn process_path_is_within(candidate: &str, root: &str) -> bool {
    let candidate = Path::new(candidate);
    let root = Path::new(root);
    candidate == root || candidate.starts_with(root)
}

#[cfg(unix)]
fn command_contains_process_path(command: &str, marker: &str) -> bool {
    command.match_indices(marker).any(|(start, matched)| {
        let end = start + matched.len();
        let before_is_boundary = command[..start]
            .chars()
            .next_back()
            .is_none_or(|value| !is_process_path_component_char(value));
        let after_is_boundary = command[end..].chars().next().is_none_or(|value| {
            matches!(value, '/' | '\\') || !is_process_path_component_char(value)
        });
        before_is_boundary && after_is_boundary
    })
}

#[cfg(unix)]
fn is_process_path_component_char(value: char) -> bool {
    value.is_alphanumeric() || matches!(value, '.' | '_' | '-' | '/' | '\\')
}

#[cfg(unix)]
#[derive(Clone, Debug)]
struct ProcessEntry {
    pid: u32,
    ppid: u32,
    command: String,
}

#[cfg(unix)]
fn process_table() -> Result<Vec<ProcessEntry>, String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .map_err(|error| format!("failed to inspect running processes: {error}"))?;
    if !output.status.success() {
        return Err("failed to inspect running processes".to_string());
    }

    let mut processes = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
            continue;
        };
        let command = parts.collect::<Vec<_>>().join(" ");
        processes.push(ProcessEntry { pid, ppid, command });
    }
    Ok(processes)
}

#[cfg(unix)]
fn current_process_identity(pid: u32) -> Result<Option<EnvDestroyProcessIdentity>, String> {
    if !process_alive(pid) {
        return Ok(None);
    }
    Ok(process_start_id(pid)?.map(|started_at| EnvDestroyProcessIdentity { pid, started_at }))
}

#[cfg(target_os = "linux")]
fn process_start_id(pid: u32) -> Result<Option<String>, String> {
    let path = format!("/proc/{pid}/stat");
    let stat = match fs::read_to_string(&path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to inspect process start identity for pid {pid}: {error}"
            ));
        }
    };
    let Some(fields) = stat.rsplit_once(')').map(|(_, fields)| fields) else {
        return Err(format!(
            "failed to parse process start identity for pid {pid}"
        ));
    };
    let Some(start_ticks) = fields.split_whitespace().nth(19) else {
        return Err(format!(
            "failed to parse process start identity for pid {pid}"
        ));
    };
    Ok(Some(start_ticks.to_string()))
}

#[cfg(target_os = "macos")]
fn process_start_id(pid: u32) -> Result<Option<String>, String> {
    const PROC_PIDTBSDINFO: i32 = 3;
    let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::zeroed();
    let size = std::mem::size_of::<ProcBsdInfo>() as i32;
    // SAFETY: `info` is a correctly sized C-compatible buffer for
    // `PROC_PIDTBSDINFO`; the return size is checked before initialization.
    let bytes = unsafe {
        proc_pidinfo(
            pid as i32,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if bytes == 0 && !process_alive(pid) {
        return Ok(None);
    }
    if bytes != size {
        return Err(format!(
            "failed to inspect process start identity for pid {pid}"
        ));
    }
    // SAFETY: `proc_pidinfo` filled the complete buffer above.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid {
        return Ok(None);
    }
    Ok(Some(format!(
        "{}:{:06}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    )))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn process_start_id(pid: u32) -> Result<Option<String>, String> {
    Err(format!(
        "process start identity inspection is unsupported for pid {pid} on this platform"
    ))
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct ProcBsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu_1: u32,
    pbi_comm: [i8; 16],
    pbi_name: [i8; 32],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

#[cfg(target_os = "macos")]
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut u8, size: i32) -> i32;
}

#[cfg(unix)]
fn process_depths(
    parent_map: &std::collections::HashMap<u32, u32>,
) -> std::collections::HashMap<u32, usize> {
    fn depth_of(
        pid: u32,
        parent_map: &std::collections::HashMap<u32, u32>,
        cache: &mut std::collections::HashMap<u32, usize>,
    ) -> usize {
        if let Some(depth) = cache.get(&pid) {
            return *depth;
        }
        let depth = match parent_map.get(&pid).copied() {
            Some(parent) if parent > 1 && parent != pid => depth_of(parent, parent_map, cache) + 1,
            _ => 0,
        };
        cache.insert(pid, depth);
        depth
    }

    let mut cache = std::collections::HashMap::new();
    for pid in parent_map.keys().copied().collect::<Vec<_>>() {
        let _ = depth_of(pid, parent_map, &mut cache);
    }
    cache
}

#[cfg(unix)]
fn interactive_shell_command(command: &str) -> bool {
    let command = command.trim_start_matches('-');
    command.ends_with("/zsh")
        || command.ends_with("/bash")
        || command.ends_with("/fish")
        || command == "zsh"
        || command == "bash"
        || command == "fish"
}

#[cfg(unix)]
fn normalize_process_path(value: &str) -> String {
    value.strip_prefix("/private").unwrap_or(value).to_string()
}

#[cfg(target_os = "linux")]
fn process_cwd_map(
    processes: &[ProcessEntry],
) -> Result<std::collections::HashMap<u32, String>, String> {
    let mut cwd_map = std::collections::HashMap::new();
    for process in processes {
        match fs::read_link(format!("/proc/{}/cwd", process.pid)) {
            Ok(path) => {
                cwd_map.insert(process.pid, path.to_string_lossy().into_owned());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) => {}
            Err(error) => {
                return Err(format!(
                    "failed to inspect process cwd for pid {}: {error}",
                    process.pid
                ));
            }
        }
    }
    Ok(cwd_map)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_cwd_map(
    _processes: &[ProcessEntry],
) -> Result<std::collections::HashMap<u32, String>, String> {
    let output = Command::new("lsof")
        .args(["-b", "-w", "-a", "-d", "cwd", "-Fpn"])
        .output()
        .map_err(|error| format!("failed to inspect process cwd: {error}"))?;
    if !output.status.success() {
        return Ok(std::collections::HashMap::new());
    }

    let mut cwd_map = std::collections::HashMap::new();
    let mut current_pid = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(pid) = line.strip_prefix('p') {
            current_pid = pid.parse::<u32>().ok();
        } else if let Some(path) = line.strip_prefix('n')
            && let Some(pid) = current_pid
        {
            cwd_map.insert(pid, path.trim().to_string());
        }
    }
    Ok(cwd_map)
}

#[cfg(unix)]
fn terminate_pid(pid: u32) -> Result<(), String> {
    let pid_text = pid.to_string();
    let _ = quiet_kill("-TERM", &pid_text);
    let deadline = Instant::now() + Duration::from_millis(750);
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return Ok(());
        }
        sleep(Duration::from_millis(50));
    }
    let _ = quiet_kill("-KILL", &pid_text);
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return Ok(());
        }
        sleep(Duration::from_millis(50));
    }
    Err(format!("failed to terminate pid {pid}"))
}

#[cfg(unix)]
fn quiet_kill(signal: &str, pid: &str) -> Result<(), String> {
    Command::new("kill")
        .args([signal, pid])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| ())
        .map_err(|error| format!("failed to run kill {signal} {pid}: {error}"))
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(status_output) = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
    else {
        return false;
    };
    if !status_output.status.success() {
        return false;
    }
    let status = String::from_utf8_lossy(&status_output.stdout);
    if status.trim().is_empty() || status.contains('Z') {
        return false;
    }

    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::HashMap;

    use super::{
        active_invocation_processes, command_contains_process_path, process_path_is_within,
    };

    #[test]
    fn process_path_containment_requires_component_boundaries() {
        assert!(process_path_is_within("/tmp/demo", "/tmp/demo"));
        assert!(process_path_is_within(
            "/tmp/demo/.openclaw/workspace",
            "/tmp/demo"
        ));
        assert!(!process_path_is_within("/tmp/demo-sibling", "/tmp/demo"));
    }

    #[test]
    fn command_path_matching_rejects_sibling_prefixes() {
        assert!(command_contains_process_path(
            "OPENCLAW_HOME=/tmp/demo node /tmp/demo/openclaw.mjs",
            "/tmp/demo"
        ));
        assert!(!command_contains_process_path(
            "node /tmp/demo-sibling/openclaw.mjs",
            "/tmp/demo"
        ));
        assert!(!command_contains_process_path(
            "node /tmp/parent-demo/openclaw.mjs",
            "/tmp/demo"
        ));
    }

    #[test]
    fn invocation_processes_include_ancestors_and_current_descendants_only() {
        let parent_map = HashMap::from([(40, 20), (20, 10), (10, 1), (30, 20), (41, 40), (42, 41)]);
        let children_map = HashMap::from([
            (1, vec![10]),
            (10, vec![20]),
            (20, vec![30, 40]),
            (40, vec![41]),
            (41, vec![42]),
        ]);

        assert_eq!(
            active_invocation_processes(40, &parent_map, &children_map),
            [10, 20, 40, 41, 42].into_iter().collect()
        );
    }
}
