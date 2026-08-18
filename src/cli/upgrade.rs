use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::{Value, json};

use super::{Cli, render};
use crate::env::{
    CloneEnvironmentOptions, CreateEnvSnapshotOptions, EnvDevMeta, RestoreEnvSnapshotOptions,
    resolve_runtime_run_dir,
};
use crate::infra::shell::{build_openclaw_dev_source_env, build_openclaw_env};
use crate::openclaw_repo::{detect_openclaw_checkout, ensure_openclaw_worktree};
use crate::runtime::releases::{
    OpenClawRelease, compare_runtime_release_versions, is_official_openclaw_releases_url,
    normalize_openclaw_channel_selector, official_openclaw_releases_url,
};
use crate::runtime::{
    InstallRuntimeFromOfficialReleaseOptions, OfficialRuntimePrepareAction, RuntimeMeta,
    RuntimeReleaseSelectorKind, RuntimeService, StagedRuntimeInstall, resolve_runtime_launch,
};
use crate::service::{ServiceSummary, wait_for_gateway_readiness};
use crate::store::{
    InstallContext, RuntimeReleaseDetails, UpgradeHistoryBinding, UpgradeHistoryRecord,
    UpgradeHistoryRuntimeRecovery, UpgradeHistoryServiceState, UpgradeHistoryStage,
    UpgradeRuntimeRecovery, clean_path, copy_dir_recursive, derive_env_paths, display_path,
    ensure_minimum_local_openclaw_config, ensure_store, get_runtime, get_upgrade_history_record,
    get_upgrade_runtime_recovery, install_runtime_from_selected_official_openclaw_release,
    list_upgrade_history, lock_env_registry, lock_upgrade_transaction, remove_runtime,
    remove_upgrade_recovery, resolve_absolute_path, runtime_install_root, runtime_integrity_issue,
    runtime_meta_path, save_environment, save_upgrade_history_record, upgrade_history_recovery_dir,
    upgrade_history_runtime_recovery_dir, write_json,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpgradeEnvSummary {
    pub env_name: String,
    pub previous_binding_kind: String,
    pub previous_binding_name: String,
    pub binding_kind: String,
    pub binding_name: String,
    pub outcome: String,
    pub runtime_release_version: Option<String>,
    pub runtime_release_channel: Option<String>,
    pub service_action: Option<String>,
    pub snapshot_id: Option<String>,
    pub rollback: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpgradeBatchSummary {
    pub count: usize,
    pub changed: usize,
    pub current: usize,
    pub skipped: usize,
    pub restarted: usize,
    pub failed: usize,
    pub results: Vec<UpgradeEnvSummary>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpgradeRollbackSummary {
    pub env_name: String,
    pub transaction_id: String,
    pub rollback_transaction_id: Option<String>,
    pub previous_binding_kind: String,
    pub previous_binding_name: String,
    pub binding_kind: String,
    pub binding_name: String,
    pub outcome: String,
    pub runtime_release_version: Option<String>,
    pub service_action: Option<String>,
    pub restored_snapshot_id: String,
    pub safety_snapshot_id: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpgradeSimulationCheck {
    pub name: String,
    pub status: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpgradeSimulationSummary {
    pub scenario: String,
    pub source_env: String,
    pub simulation_env: String,
    pub from_binding_kind: String,
    pub from_binding_name: String,
    pub to_binding_kind: String,
    pub to_binding_name: String,
    pub to: String,
    pub outcome: String,
    pub checks: Vec<UpgradeSimulationCheck>,
    pub cleanup_command: String,
    pub cleanup: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpgradeSimulationBatchSummary {
    pub source_env: String,
    pub to: String,
    pub count: usize,
    pub passed: usize,
    pub failed: usize,
    pub results: Vec<UpgradeSimulationSummary>,
}

#[derive(Clone, Debug)]
struct UpgradeTarget {
    version: Option<String>,
    channel: Option<String>,
    runtime: Option<String>,
}

#[derive(Clone, Debug)]
enum ResolvedUpgradeTargetKind {
    Named(Box<RuntimeMeta>),
    Official(OpenClawRelease),
}

#[derive(Clone, Debug)]
struct ResolvedUpgradeTarget {
    name: String,
    release_version: Option<String>,
    release_channel: Option<String>,
    kind: ResolvedUpgradeTargetKind,
}

#[derive(Clone, Debug)]
enum UpgradeSimulationTarget {
    Official {
        target: UpgradeTarget,
        display: String,
    },
    LocalRepo {
        repo_root: PathBuf,
        display: String,
    },
}

#[derive(Clone, Copy, Debug)]
enum UpgradeSimulationScenario {
    Current,
    Minimum,
    Telegram,
}

#[derive(Clone, Copy, Debug)]
struct UpgradeOptions {
    dry_run: bool,
    rollback_enabled: bool,
}

#[derive(Clone, Copy, Debug)]
struct UpgradeSimulationOptions {
    keep_envs: bool,
}

#[derive(Clone, Debug)]
struct UpgradeTransactionPlan {
    source: UpgradeHistoryBinding,
    target: UpgradeHistoryBinding,
}

#[derive(Clone, Debug)]
struct UpgradeRollbackPlan {
    record: UpgradeHistoryRecord,
    recovery: Option<UpgradeRuntimeRecovery>,
}

#[derive(Clone, Debug)]
struct PreparedSimulationRuntime {
    name: String,
    note: String,
    temporary: bool,
}

impl UpgradeTarget {
    fn parse(args: Vec<String>) -> Result<(Vec<String>, Self), String> {
        let (args, version) = Cli::consume_option(args, "--version")?;
        let version = Cli::require_option_value(version, "--version")?;
        let (args, channel) = Cli::consume_option(args, "--channel")?;
        let channel = Cli::require_option_value(channel, "--channel")?;
        let (args, runtime) = Cli::consume_option(args, "--runtime")?;
        let runtime = Cli::require_option_value(runtime, "--runtime")?;
        let explicit_count = usize::from(version.is_some())
            + usize::from(channel.is_some())
            + usize::from(runtime.is_some());
        if explicit_count > 1 {
            return Err(
                "upgrade accepts only one of --version, --channel, or --runtime".to_string(),
            );
        }
        Ok((
            args,
            Self {
                version,
                channel,
                runtime,
            },
        ))
    }

    fn is_explicit(&self) -> bool {
        self.version.is_some() || self.channel.is_some() || self.runtime.is_some()
    }

    fn canonical_runtime_name(&self) -> Result<String, String> {
        if let Some(runtime) = self.runtime.as_deref() {
            return Ok(runtime.to_string());
        }
        RuntimeService::canonical_official_openclaw_runtime_name(
            self.version.as_deref(),
            self.channel.as_deref(),
        )
    }

    fn release_channel_hint(&self) -> Option<String> {
        self.channel.clone()
    }

    fn is_named_runtime(&self) -> bool {
        self.runtime.is_some()
    }
}

impl Cli {
    pub(super) fn upgrade_env_to_runtime_target(
        &self,
        name: &str,
        version: Option<String>,
        channel: Option<String>,
        runtime: Option<String>,
    ) -> Result<UpgradeEnvSummary, String> {
        let explicit_count = usize::from(version.is_some())
            + usize::from(channel.is_some())
            + usize::from(runtime.is_some());
        if explicit_count != 1 {
            return Err(
                "runtime transition requires exactly one version, channel, or runtime".to_string(),
            );
        }
        self.upgrade_env(
            name,
            &UpgradeTarget {
                version,
                channel,
                runtime,
            },
            UpgradeOptions {
                dry_run: false,
                rollback_enabled: true,
            },
        )
    }

    pub(super) fn handle_upgrade_command(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "upgrade")?;
        if matches!(args.first().map(String::as_str), Some("rollback")) {
            return self.handle_upgrade_rollback(args[1..].to_vec(), json_flag, profile);
        }
        if matches!(args.first().map(String::as_str), Some("history")) {
            let Some(env_name) = args.get(1) else {
                return Err("upgrade history requires <env>".to_string());
            };
            Self::assert_no_extra_args(&args[2..])?;
            let history = list_upgrade_history(env_name, &self.env, &self.cwd)?;
            if json_flag {
                self.print_json(&history)?;
            } else {
                self.stdout_lines(render::upgrade::upgrade_history(
                    env_name, &history, profile,
                )?);
            }
            return Ok(0);
        }
        if matches!(args.first().map(String::as_str), Some("simulate")) {
            let summaries = self.upgrade_simulate(args[1..].to_vec())?;
            let failed = summaries.iter().any(|summary| summary.outcome == "failed");
            if json_flag {
                if summaries.len() == 1 {
                    self.print_json(&summaries[0])?;
                } else {
                    self.print_json(&build_simulation_batch_summary(summaries))?;
                }
                return Ok(if failed { 1 } else { 0 });
            }
            if summaries.len() == 1 {
                self.stdout_lines(render::upgrade::upgrade_simulation(
                    &summaries[0],
                    profile,
                    &self.command_example(),
                ));
            } else {
                self.stdout_lines(render::upgrade::upgrade_simulation_batch(
                    &build_simulation_batch_summary(summaries),
                    profile,
                    &self.command_example(),
                ));
            }
            return Ok(if failed { 1 } else { 0 });
        }

        let (args, dry_run) = Self::consume_flag(args, "--dry-run");
        let (args, no_rollback) = Self::consume_flag(args, "--no-rollback");
        let (args, all_flag) = Self::consume_flag(args, "--all");
        let (args, target) = UpgradeTarget::parse(args)?;
        let options = UpgradeOptions {
            dry_run,
            rollback_enabled: !no_rollback,
        };

        if all_flag {
            Self::assert_no_extra_args(&args)?;
            if target.is_explicit() {
                return Err(
                    "upgrade --all does not accept --version, --channel, or --runtime; upgrade one env at a time when changing selectors"
                        .to_string(),
                );
            }

            let envs = self.environment_service().list()?;
            let mut results = Vec::with_capacity(envs.len());
            for env in envs {
                match self.upgrade_env(&env.name, &target, options) {
                    Ok(summary) => results.push(summary),
                    Err(error) => results.push(UpgradeEnvSummary {
                        env_name: env.name,
                        previous_binding_kind: "unknown".to_string(),
                        previous_binding_name: "—".to_string(),
                        binding_kind: "unknown".to_string(),
                        binding_name: "—".to_string(),
                        outcome: "failed".to_string(),
                        runtime_release_version: None,
                        runtime_release_channel: None,
                        service_action: None,
                        snapshot_id: None,
                        rollback: None,
                        note: Some(error),
                    }),
                }
            }

            let summary = UpgradeBatchSummary {
                count: results.len(),
                changed: results
                    .iter()
                    .filter(|summary| is_changed_upgrade_outcome(&summary.outcome))
                    .count(),
                current: results
                    .iter()
                    .filter(|summary| summary.outcome == "up-to-date")
                    .count(),
                skipped: results
                    .iter()
                    .filter(|summary| {
                        matches!(
                            summary.outcome.as_str(),
                            "pinned" | "local-command" | "manual-runtime"
                        )
                    })
                    .count(),
                restarted: results
                    .iter()
                    .filter(|summary| summary.service_action.is_some())
                    .count(),
                failed: results
                    .iter()
                    .filter(|summary| is_failed_upgrade_outcome(&summary.outcome))
                    .count(),
                results,
            };

            if json_flag {
                self.print_json(&summary)?;
                return Ok(if summary.failed == 0 { 0 } else { 1 });
            }

            self.stdout_lines(render::upgrade::upgrade_batch(
                &summary,
                profile,
                &self.command_example(),
            ));
            return Ok(if summary.failed == 0 { 0 } else { 1 });
        }

        let Some(name) = args.first() else {
            return Err("upgrade requires <env> or --all".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary = self.upgrade_env(name, &target, options)?;
        let failed = is_failed_upgrade_outcome(&summary.outcome);
        if json_flag {
            self.print_json(&summary)?;
            return Ok(if failed { 1 } else { 0 });
        }

        self.stdout_lines(render::upgrade::upgrade_env(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(if failed { 1 } else { 0 })
    }

    fn handle_upgrade_rollback(
        &self,
        args: Vec<String>,
        json_flag: bool,
        profile: render::RenderProfile,
    ) -> Result<i32, String> {
        let (args, dry_run) = Self::consume_flag(args, "--dry-run");
        let (args, transaction_id) = Self::consume_option(args, "--transaction")?;
        let transaction_id = Self::require_option_value(transaction_id, "--transaction")?;
        let Some(env_name) = args.first() else {
            return Err("upgrade rollback requires <env>".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary =
            self.rollback_completed_upgrade(env_name, transaction_id.as_deref(), dry_run)?;
        let failed = matches!(summary.outcome.as_str(), "failed" | "rollback-failed");
        if json_flag {
            self.print_json(&summary)?;
        } else {
            self.stdout_lines(render::upgrade::upgrade_rollback(&summary, profile));
        }
        Ok(if failed { 1 } else { 0 })
    }

    fn rollback_completed_upgrade(
        &self,
        env_name: &str,
        transaction_id: Option<&str>,
        dry_run: bool,
    ) -> Result<UpgradeRollbackSummary, String> {
        let plan = self.prepare_upgrade_rollback(env_name, transaction_id)?;
        if dry_run {
            return Ok(UpgradeRollbackSummary {
                env_name: env_name.to_string(),
                transaction_id: plan.record.id.clone(),
                rollback_transaction_id: None,
                previous_binding_kind: plan.record.target.kind.clone(),
                previous_binding_name: plan.record.target.name.clone(),
                binding_kind: plan.record.source.kind.clone(),
                binding_name: plan.record.source.name.clone(),
                outcome: "would-rollback".to_string(),
                runtime_release_version: plan.record.source.openclaw_version.clone(),
                service_action: rollback_service_action_for_dry_run(&plan.record),
                restored_snapshot_id: plan.record.snapshot_id.clone(),
                safety_snapshot_id: None,
                note: Some(
                    "dry run: no runtime, env, service, snapshot, or history changed".to_string(),
                ),
            });
        }

        let _transaction_lock = lock_upgrade_transaction(env_name, &self.env, &self.cwd)?;
        let _operation_lock = self.environment_service().lock_operation(env_name)?;
        let plan = self.prepare_upgrade_rollback(env_name, Some(&plan.record.id))?;
        self.execute_upgrade_rollback_locked(env_name, plan)
    }

    fn prepare_upgrade_rollback(
        &self,
        env_name: &str,
        transaction_id: Option<&str>,
    ) -> Result<UpgradeRollbackPlan, String> {
        let history = list_upgrade_history(env_name, &self.env, &self.cwd)?;
        let record = match transaction_id {
            Some(transaction_id) => {
                get_upgrade_history_record(env_name, transaction_id, &self.env, &self.cwd)?
            }
            None => history
                .iter()
                .find(|record| {
                    is_rollback_candidate(record)
                        && !has_successful_rollback_child(&history, &record.id)
                })
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "environment \"{env_name}\" does not have a completed upgrade transaction available to roll back"
                    )
                })?,
        };
        if !is_rollback_candidate(&record) {
            return Err(format!(
                "upgrade transaction \"{}\" cannot be rolled back because its outcome is \"{}\"",
                record.id, record.outcome
            ));
        }
        if has_successful_rollback_child(&history, &record.id) {
            return Err(format!(
                "upgrade transaction \"{}\" has already been rolled back",
                record.id
            ));
        }

        let current = self.environment_service().get(env_name)?;
        let current_binding = source_binding(&current);
        if current_binding.0 != record.target.kind || current_binding.1 != record.target.name {
            return Err(format!(
                "refusing to roll back upgrade transaction \"{}\": env \"{env_name}\" now uses {}:{}, expected {}:{}",
                record.id,
                current_binding.0,
                current_binding.1,
                record.target.kind,
                record.target.name
            ));
        }
        if current.service_enabled != record.service_after.enabled
            || current.service_running != record.service_after.running
        {
            return Err(format!(
                "refusing to roll back upgrade transaction \"{}\": service policy changed after the transaction",
                record.id
            ));
        }
        self.environment_service()
            .get_snapshot(env_name, &record.snapshot_id)
            .map_err(|error| {
                format!(
                    "cannot roll back upgrade transaction \"{}\": {error}",
                    record.id
                )
            })?;
        self.verify_rollback_target_version(env_name, &record)?;
        let recovery = self.verify_rollback_source(env_name, &record)?;
        Ok(UpgradeRollbackPlan { record, recovery })
    }

    fn verify_rollback_target_version(
        &self,
        env_name: &str,
        record: &UpgradeHistoryRecord,
    ) -> Result<(), String> {
        let Some(expected_version) = record.target.openclaw_version.as_deref() else {
            return Ok(());
        };
        let version =
            self.run_openclaw_command(env_name, "current openclaw --version", &["--version"])?;
        if version_output_matches_expected(version.first_line().trim(), expected_version) {
            return Ok(());
        }
        Err(format!(
            "refusing to roll back upgrade transaction \"{}\": env \"{env_name}\" reports OpenClaw {}, expected {}",
            record.id,
            version.first_line().trim(),
            expected_version
        ))
    }

    fn verify_rollback_source(
        &self,
        env_name: &str,
        record: &UpgradeHistoryRecord,
    ) -> Result<Option<UpgradeRuntimeRecovery>, String> {
        match record.source.kind.as_str() {
            "runtime" => {
                if record.target.kind == "runtime" && record.source.name == record.target.name {
                    self.ensure_runtime_upgrade_isolated(env_name, &record.source.name)?;
                    let recovery_entry = record
                        .runtime_recovery
                        .iter()
                        .find(|recovery| {
                            recovery.runtime_name == record.source.name
                                && recovery.backup_id.is_some()
                        })
                        .ok_or_else(|| {
                            format!(
                                "cannot roll back upgrade transaction \"{}\": retained runtime recovery for \"{}\" is unavailable",
                                record.id, record.source.name
                            )
                        })?;
                    if recovery_entry.backup_id.as_deref() != Some(record.source.name.as_str()) {
                        return Err(format!(
                            "cannot roll back upgrade transaction \"{}\": retained runtime recovery id does not match \"{}\"",
                            record.id, record.source.name
                        ));
                    }
                    let recovery = get_upgrade_runtime_recovery(
                        env_name,
                        &record.id,
                        &record.snapshot_id,
                        &record.source.name,
                        &self.env,
                        &self.cwd,
                    )
                    .map_err(|error| {
                        format!(
                            "cannot roll back upgrade transaction \"{}\": {error}",
                            record.id
                        )
                    })?;
                    if let Some(expected_version) = record.source.openclaw_version.as_deref()
                        && recovery.meta.release_version.as_deref() != Some(expected_version)
                    {
                        return Err(format!(
                            "cannot roll back upgrade transaction \"{}\": retained runtime \"{}\" is OpenClaw {}, expected {}",
                            record.id,
                            record.source.name,
                            recovery
                                .meta
                                .release_version
                                .as_deref()
                                .unwrap_or("unknown"),
                            expected_version
                        ));
                    }
                    return Ok(Some(recovery));
                }

                let runtime =
                    get_runtime(&record.source.name, &self.env, &self.cwd).map_err(|error| {
                        format!(
                            "cannot roll back upgrade transaction \"{}\": {error}",
                            record.id
                        )
                    })?;
                if let Some(issue) = runtime_integrity_issue(&runtime, &self.env) {
                    return Err(format!(
                        "cannot roll back upgrade transaction \"{}\": runtime \"{}\" is not healthy: {issue}",
                        record.id, record.source.name
                    ));
                }
                if let Some(expected_version) = record.source.openclaw_version.as_deref() {
                    let version = self.run_update_mode_openclaw_command_output(
                        env_name,
                        &record.source.name,
                        "rollback source openclaw --version",
                        &["--version"],
                    )?;
                    if !version.status.success()
                        || !version_output_matches_expected(
                            version.first_line().trim(),
                            expected_version,
                        )
                    {
                        return Err(format!(
                            "cannot roll back upgrade transaction \"{}\": runtime \"{}\" does not report OpenClaw {}",
                            record.id, record.source.name, expected_version
                        ));
                    }
                }
                Ok(None)
            }
            "launcher" => {
                let version = self.run_launcher_mode_openclaw_command_output(
                    env_name,
                    &record.source.name,
                    "rollback source openclaw --version",
                    &["--version"],
                )?;
                if !version.status.success() {
                    return Err(format!(
                        "cannot roll back upgrade transaction \"{}\": launcher \"{}\" failed OpenClaw version verification: {}",
                        record.id,
                        record.source.name,
                        version.failure_summary()
                    ));
                }
                if let Some(expected_version) = record.source.openclaw_version.as_deref()
                    && !version_output_matches_expected(
                        version.first_line().trim(),
                        expected_version,
                    )
                {
                    return Err(format!(
                        "cannot roll back upgrade transaction \"{}\": launcher \"{}\" does not report OpenClaw {}",
                        record.id, record.source.name, expected_version
                    ));
                }
                Ok(None)
            }
            source_kind => Err(format!(
                "cannot roll back upgrade transaction \"{}\": source binding kind \"{source_kind}\" is not supported",
                record.id
            )),
        }
    }

    fn execute_upgrade_rollback_locked(
        &self,
        env_name: &str,
        plan: UpgradeRollbackPlan,
    ) -> Result<UpgradeRollbackSummary, String> {
        let runtime_names = plan
            .recovery
            .as_ref()
            .map(|recovery| vec![recovery.meta.name.clone()])
            .unwrap_or_default();
        let mut transaction = self.begin_upgrade_transaction_locked(
            env_name,
            UpgradeTransactionPlan {
                source: plan.record.target.clone(),
                target: plan.record.source.clone(),
            },
            &runtime_names,
            true,
            "pre-rollback",
            Some(plan.record.id.clone()),
        )?;
        let rollback_transaction_id = transaction.id.clone();
        let safety_snapshot_id = transaction.snapshot_id.clone();

        if plan.record.service_after.enabled
            && plan.record.service_after.running
            && let Err(error) = self
                .service_service()
                .quiesce_for_runtime_mutation_locked(env_name)
        {
            return Ok(self.fail_upgrade_rollback_locked(
                env_name,
                &plan,
                transaction,
                format!("failed to quiesce the managed service before rollback: {error}"),
            ));
        }

        if let Some(recovery) = plan.recovery.as_ref() {
            transaction.mark_runtime_mutated(&recovery.meta.name);
            if let Err(error) = self.restore_retained_runtime(recovery) {
                return Ok(self.fail_upgrade_rollback_locked(env_name, &plan, transaction, error));
            }
        }

        if let Err(error) =
            self.environment_service()
                .restore_snapshot_locked(RestoreEnvSnapshotOptions {
                    env_name: env_name.to_string(),
                    snapshot_id: plan.record.snapshot_id.clone(),
                })
        {
            return Ok(self.fail_upgrade_rollback_locked(
                env_name,
                &plan,
                transaction,
                format!("failed to restore the recorded pre-upgrade snapshot: {error}"),
            ));
        }

        let service_action = match self.reconcile_rolled_back_service_locked(env_name, &plan.record)
        {
            Ok(action) => action,
            Err(error) => {
                return Ok(self.fail_upgrade_rollback_locked(env_name, &plan, transaction, error));
            }
        };
        let verification_note = match self.verify_upgraded_openclaw(
            env_name,
            plan.record.source.openclaw_version.as_deref(),
            plan.record.service_before.running,
        ) {
            Ok(note) => note,
            Err(error) => {
                return Ok(self.fail_upgrade_rollback_locked(
                    env_name,
                    &plan,
                    transaction,
                    format!("post-rollback verification failed: {error}"),
                ));
            }
        };
        transaction.mark_post_update_not_needed();

        let history_summary = UpgradeEnvSummary {
            env_name: env_name.to_string(),
            previous_binding_kind: plan.record.target.kind.clone(),
            previous_binding_name: plan.record.target.name.clone(),
            binding_kind: plan.record.source.kind.clone(),
            binding_name: plan.record.source.name.clone(),
            outcome: "rolled-back".to_string(),
            runtime_release_version: plan.record.source.openclaw_version.clone(),
            runtime_release_channel: None,
            service_action: service_action.clone(),
            snapshot_id: Some(safety_snapshot_id.clone()),
            rollback: None,
            note: verification_note.clone(),
        };
        if let Err(error) = self.retain_required_runtime_recovery(env_name, &mut transaction) {
            return Ok(self.fail_upgrade_rollback_locked(
                env_name,
                &plan,
                transaction,
                format!("failed to retain pre-rollback runtime recovery: {error}"),
            ));
        }
        if let Err(error) = self.record_upgrade_history(&transaction, &history_summary) {
            return Ok(self.fail_upgrade_rollback_locked(
                env_name,
                &plan,
                transaction,
                format!("failed to record rollback history: {error}"),
            ));
        }
        transaction.commit();

        let cleanup_note = remove_upgrade_recovery(env_name, &plan.record.id, &self.env, &self.cwd)
            .err()
            .map(|error| format!("Original recovery cleanup requires attention: {error}"));

        Ok(UpgradeRollbackSummary {
            env_name: env_name.to_string(),
            transaction_id: plan.record.id,
            rollback_transaction_id: Some(rollback_transaction_id),
            previous_binding_kind: plan.record.target.kind,
            previous_binding_name: plan.record.target.name,
            binding_kind: plan.record.source.kind,
            binding_name: plan.record.source.name,
            outcome: "rolled-back".to_string(),
            runtime_release_version: plan.record.source.openclaw_version,
            service_action,
            restored_snapshot_id: plan.record.snapshot_id,
            safety_snapshot_id: Some(safety_snapshot_id),
            note: join_optional_warnings(verification_note, cleanup_note),
        })
    }

    fn restore_retained_runtime(&self, recovery: &UpgradeRuntimeRecovery) -> Result<(), String> {
        let install_root = runtime_install_root(&recovery.meta.name, &self.env, &self.cwd)?;
        if install_root.exists() {
            fs::remove_dir_all(&install_root).map_err(|error| {
                format!(
                    "failed to remove current runtime root {}: {error}",
                    display_path(&install_root)
                )
            })?;
        }
        copy_dir_recursive(&recovery.install_root, &install_root)?;
        let meta_path = runtime_meta_path(&recovery.meta.name, &self.env, &self.cwd)?;
        write_json(&meta_path, &recovery.meta)
    }

    fn reconcile_rolled_back_service_locked(
        &self,
        env_name: &str,
        record: &UpgradeHistoryRecord,
    ) -> Result<Option<String>, String> {
        if !record.service_before.enabled || !record.service_before.running {
            return Ok(None);
        }
        let started = self
            .with_progress(format!("Starting restored service for {env_name}"), || {
                self.service_service().start_locked(env_name)
            })?;
        self.wait_for_restarted_gateway_health(env_name, started.desired_running)?;
        Ok(Some("started".to_string()))
    }

    fn fail_upgrade_rollback_locked(
        &self,
        env_name: &str,
        plan: &UpgradeRollbackPlan,
        transaction: UpgradeTransaction,
        error: String,
    ) -> UpgradeRollbackSummary {
        let rollback_transaction_id = transaction.id.clone();
        let safety_snapshot_id = transaction.snapshot_id.clone();
        let restore_result = self.rollback_upgrade_locked(env_name, &transaction);
        let restored_pre_rollback_state = restore_result.is_ok();
        let (outcome, rollback, note) = match restore_result {
            Ok(()) => (
                "failed".to_string(),
                "restored".to_string(),
                format!("rollback failed, so ocm restored the pre-rollback state: {error}"),
            ),
            Err(restore_error) => (
                "rollback-failed".to_string(),
                "failed".to_string(),
                format!(
                    "rollback failed ({error}); restoring the pre-rollback state also failed: {restore_error}"
                ),
            ),
        };
        let history_summary = UpgradeEnvSummary {
            env_name: env_name.to_string(),
            previous_binding_kind: plan.record.target.kind.clone(),
            previous_binding_name: plan.record.target.name.clone(),
            binding_kind: plan.record.source.kind.clone(),
            binding_name: plan.record.source.name.clone(),
            outcome: outcome.clone(),
            runtime_release_version: plan.record.source.openclaw_version.clone(),
            runtime_release_channel: None,
            service_action: None,
            snapshot_id: Some(safety_snapshot_id.clone()),
            rollback: Some(rollback),
            note: Some(note.clone()),
        };
        let history_error = self
            .record_upgrade_history(&transaction, &history_summary)
            .err();
        let mut retained_safety_snapshot_id = Some(safety_snapshot_id.clone());
        let snapshot_cleanup_note = if history_error.is_some() && restored_pre_rollback_state {
            match self.environment_service().remove_snapshot_locked(
                crate::env::RemoveEnvSnapshotOptions {
                    env_name: env_name.to_string(),
                    snapshot_id: safety_snapshot_id.clone(),
                },
            ) {
                Ok(removed) => {
                    retained_safety_snapshot_id = None;
                    join_optional_warnings(
                        Some("Unrecorded safety snapshot was removed.".to_string()),
                        join_warnings(&removed.warnings).map(|warning| {
                            format!("Snapshot cleanup requires attention: {warning}")
                        }),
                    )
                }
                Err(error) => Some(format!(
                    "Unrecorded safety snapshot cleanup failed: {error}"
                )),
            }
        } else if history_error.is_some() {
            Some(format!(
                "Safety snapshot \"{safety_snapshot_id}\" was retained because restoring the pre-rollback state failed."
            ))
        } else {
            None
        };
        transaction.cleanup();

        UpgradeRollbackSummary {
            env_name: env_name.to_string(),
            transaction_id: plan.record.id.clone(),
            rollback_transaction_id: Some(rollback_transaction_id),
            previous_binding_kind: plan.record.target.kind.clone(),
            previous_binding_name: plan.record.target.name.clone(),
            binding_kind: plan.record.source.kind.clone(),
            binding_name: plan.record.source.name.clone(),
            outcome,
            runtime_release_version: plan.record.source.openclaw_version.clone(),
            service_action: None,
            restored_snapshot_id: plan.record.snapshot_id.clone(),
            safety_snapshot_id: retained_safety_snapshot_id,
            note: join_optional_warnings(
                join_optional_warnings(
                    Some(note),
                    history_error
                        .map(|error| format!("Rollback history was not recorded: {error}")),
                ),
                snapshot_cleanup_note,
            ),
        }
    }

    fn upgrade_simulate(&self, args: Vec<String>) -> Result<Vec<UpgradeSimulationSummary>, String> {
        let (args, keep_simulations) = Self::consume_flag(args, "--keep-simulations");
        let (args, keep_simulation) = Self::consume_flag(args, "--keep-simulation");
        let options = UpgradeSimulationOptions {
            keep_envs: keep_simulations || keep_simulation,
        };
        let (args, to) = Self::consume_option(args, "--to")?;
        let to = Self::require_option_value(to, "--to")?.ok_or_else(|| {
            "upgrade simulate requires --to <version|channel|repo-path>".to_string()
        })?;
        let (args, scenario) = Self::consume_option(args, "--scenario")?;
        let scenario = Self::require_option_value(scenario, "--scenario")?;
        let scenarios = UpgradeSimulationScenario::parse_many(scenario.as_deref())?;
        let Some(source_name) = args.first() else {
            return Err("upgrade simulate requires an environment name".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;
        self.environment_service().get(source_name)?;

        let target = self.resolve_simulation_target(&to)?;
        self.validate_simulation_target(&target)?;
        let prepared_runtime = self.prepare_shared_simulation_runtime(source_name, &target)?;
        let mut summaries = Vec::with_capacity(scenarios.len());
        for scenario in scenarios {
            match self.upgrade_simulate_one(
                source_name,
                &target,
                prepared_runtime.as_ref(),
                scenario,
                options,
            ) {
                Ok(summary) => summaries.push(summary),
                Err(error) => {
                    self.finish_shared_simulation_runtime(
                        &mut summaries,
                        prepared_runtime.as_ref(),
                        options,
                    )?;
                    return Err(error);
                }
            }
        }
        self.finish_shared_simulation_runtime(&mut summaries, prepared_runtime.as_ref(), options)?;
        Ok(summaries)
    }

    fn upgrade_simulate_one(
        &self,
        source_name: &str,
        target: &UpgradeSimulationTarget,
        prepared_runtime: Option<&PreparedSimulationRuntime>,
        scenario: UpgradeSimulationScenario,
        options: UpgradeSimulationOptions,
    ) -> Result<UpgradeSimulationSummary, String> {
        let source = self.environment_service().get(source_name)?;
        let (from_binding_kind, from_binding_name) = source_binding(&source);
        let simulation_name = simulation_env_name(source_name, scenario.id());
        let cloned = self
            .environment_service()
            .clone_for_simulation(CloneEnvironmentOptions {
                source_name: source_name.to_string(),
                name: simulation_name.clone(),
                root: None,
            })?;
        if let Err(error) =
            self.environment_service()
                .set_service_policy(&cloned.name, Some(false), Some(false))
        {
            let _ = self.environment_service().remove(&cloned.name, true);
            return Err(error);
        }
        if cloned.protected
            && let Err(error) = self
                .environment_service()
                .set_protected(&cloned.name, false)
        {
            let _ = self.environment_service().remove(&cloned.name, true);
            return Err(error);
        }

        let mut checks = vec![UpgradeSimulationCheck::passed(
            "clone env",
            format!("created isolated env {}", cloned.name),
        )];
        let mut to_binding_kind = "unknown".to_string();
        let mut to_binding_name = "unknown".to_string();

        let scenario_check = self.apply_simulation_scenario(&cloned.name, scenario);
        let scenario_failed = scenario_check.status == "failed";
        checks.push(scenario_check);
        if scenario_failed {
            let summary = self.build_simulation_summary(
                source_name,
                &cloned.name,
                from_binding_kind,
                from_binding_name,
                to_binding_kind,
                to_binding_name,
                scenario,
                target.display(),
                checks,
            );
            return self.finish_simulation_summary(summary, options);
        }
        checks.push(self.run_update_plan_check(&cloned.name, target));

        match self.apply_simulation_target(&cloned.name, target, prepared_runtime) {
            Ok((kind, name, note)) => {
                to_binding_kind = kind;
                to_binding_name = name;
                checks.push(UpgradeSimulationCheck::passed("prepare target", note));
            }
            Err(error) => {
                checks.push(UpgradeSimulationCheck::failed("prepare target", error));
                let summary = self.build_simulation_summary(
                    source_name,
                    &cloned.name,
                    from_binding_kind,
                    from_binding_name,
                    to_binding_kind,
                    to_binding_name,
                    scenario,
                    target.display(),
                    checks,
                );
                return self.finish_simulation_summary(summary, options);
            }
        }

        if matches!(target, UpgradeSimulationTarget::LocalRepo { .. }) {
            checks.push(self.run_local_repo_script_check(&cloned.name, "pnpm build", "build"));
            checks.push(self.run_local_repo_script_check(
                &cloned.name,
                "pnpm ui:build",
                "ui:build",
            ));
        }

        checks.push(self.run_simulation_check(&cloned.name, "openclaw --version", &["--version"]));
        checks.push(self.run_simulation_check_with_env(
            &cloned.name,
            "openclaw doctor",
            &["doctor", "--non-interactive", "--fix"],
            &[("OPENCLAW_UPDATE_IN_PROGRESS", "1")],
        ));
        checks.push(self.run_simulation_check(
            &cloned.name,
            "openclaw plugins update",
            &["plugins", "update", "--all", "--dry-run"],
        ));
        checks.push(self.run_simulation_check(
            &cloned.name,
            "openclaw gateway status",
            &["gateway", "status", "--deep", "--json"],
        ));

        let summary = self.build_simulation_summary(
            source_name,
            &cloned.name,
            from_binding_kind,
            from_binding_name,
            to_binding_kind,
            to_binding_name,
            scenario,
            target.display(),
            checks,
        );
        self.finish_simulation_summary(summary, options)
    }

    fn apply_simulation_scenario(
        &self,
        simulation_name: &str,
        scenario: UpgradeSimulationScenario,
    ) -> UpgradeSimulationCheck {
        match self.seed_simulation_scenario(simulation_name, scenario) {
            Ok(note) => UpgradeSimulationCheck::passed("seed scenario", note),
            Err(error) => UpgradeSimulationCheck::failed("seed scenario", error),
        }
    }

    fn seed_simulation_scenario(
        &self,
        simulation_name: &str,
        scenario: UpgradeSimulationScenario,
    ) -> Result<String, String> {
        let meta = self
            .environment_service()
            .apply_effective_gateway_port(self.environment_service().get(simulation_name)?)?;
        let gateway_port = meta.gateway_port.unwrap_or_default();
        let paths = derive_env_paths(Path::new(&meta.root));
        match scenario {
            UpgradeSimulationScenario::Current => Ok("using source env config".to_string()),
            UpgradeSimulationScenario::Minimum => {
                reset_to_minimum_simulation_config(&paths, gateway_port)?;
                Ok("seeded minimum OpenClaw config".to_string())
            }
            UpgradeSimulationScenario::Telegram => {
                reset_to_minimum_simulation_config(&paths, gateway_port)?;
                seed_telegram_simulation_config(&paths)?;
                Ok("seeded Telegram channel/plugin config".to_string())
            }
        }
    }

    fn run_update_plan_check(
        &self,
        simulation_name: &str,
        target: &UpgradeSimulationTarget,
    ) -> UpgradeSimulationCheck {
        let Some(update_args) = target.update_plan_args() else {
            return UpgradeSimulationCheck::skipped(
                "openclaw update plan",
                "local repo targets are validated through checkout build and post-update checks",
            );
        };
        let refs = update_args.iter().map(String::as_str).collect::<Vec<_>>();
        self.run_simulation_check(simulation_name, "openclaw update plan", &refs)
    }

    fn resolve_simulation_target(&self, to: &str) -> Result<UpgradeSimulationTarget, String> {
        let path = resolve_absolute_path(to, &self.env, &self.cwd)?;
        if let Some(repo_root) = detect_openclaw_checkout(&path) {
            return Ok(UpgradeSimulationTarget::LocalRepo {
                display: display_path(&repo_root),
                repo_root,
            });
        }

        let trimmed = to.trim();
        if matches!(trimmed, "stable" | "latest" | "beta" | "dev") {
            return Ok(UpgradeSimulationTarget::Official {
                target: UpgradeTarget {
                    version: None,
                    channel: Some(normalize_openclaw_channel_selector(trimmed)?),
                    runtime: None,
                },
                display: trimmed.to_string(),
            });
        }

        Ok(UpgradeSimulationTarget::Official {
            target: UpgradeTarget {
                version: Some(trimmed.to_string()),
                channel: None,
                runtime: None,
            },
            display: trimmed.to_string(),
        })
    }

    fn validate_simulation_target(&self, target: &UpgradeSimulationTarget) -> Result<(), String> {
        let UpgradeSimulationTarget::Official { target, .. } = target else {
            return Ok(());
        };

        let releases = self
            .runtime_service()
            .official_openclaw_releases(None, None)?;
        match (target.version.as_deref(), target.channel.as_deref()) {
            (Some(version), None) => {
                if releases.iter().any(|release| release.version == version) {
                    Ok(())
                } else {
                    Err(missing_simulation_version_error(version, &releases))
                }
            }
            (None, Some(channel)) => {
                if releases
                    .iter()
                    .any(|release| release.channel.as_deref() == Some(channel))
                {
                    Ok(())
                } else {
                    Err(format!(
                        "OpenClaw release channel \"{channel}\" was not found; simulation did not create any scenario envs"
                    ))
                }
            }
            _ => Err(
                "upgrade simulate requires a published version, channel, or local repo path"
                    .to_string(),
            ),
        }
    }

    fn apply_simulation_target(
        &self,
        simulation_name: &str,
        target: &UpgradeSimulationTarget,
        prepared_runtime: Option<&PreparedSimulationRuntime>,
    ) -> Result<(String, String, String), String> {
        match target {
            UpgradeSimulationTarget::Official { .. } => {
                let prepared_runtime = prepared_runtime.ok_or_else(|| {
                    "simulation target runtime was not prepared before scenario execution"
                        .to_string()
                })?;
                self.environment_service()
                    .set_runtime(simulation_name, &prepared_runtime.name)?;
                Ok((
                    "runtime".to_string(),
                    prepared_runtime.name.clone(),
                    prepared_runtime.note.clone(),
                ))
            }
            UpgradeSimulationTarget::LocalRepo { repo_root, .. } => {
                let worktree_root = ensure_openclaw_worktree(repo_root, simulation_name)?;
                let mut meta = self.environment_service().get(simulation_name)?;
                meta.default_runtime = None;
                meta.default_launcher = None;
                meta.dev = Some(EnvDevMeta {
                    repo_root: display_path(repo_root),
                    worktree_root: display_path(&worktree_root),
                });
                let mut meta = save_environment(meta, &self.env, &self.cwd)?;
                meta = self
                    .environment_service()
                    .apply_effective_gateway_port(meta)?;
                let paths = derive_env_paths(Path::new(&meta.root));
                ensure_minimum_local_openclaw_config(
                    &paths,
                    meta.gateway_port.unwrap_or_default(),
                )?;
                self.ensure_simulation_dev_dependencies(&meta)?;
                Ok((
                    "dev".to_string(),
                    "local-repo".to_string(),
                    format!("prepared local repo {}", display_path(repo_root)),
                ))
            }
        }
    }

    fn prepare_shared_simulation_runtime(
        &self,
        source_name: &str,
        target: &UpgradeSimulationTarget,
    ) -> Result<Option<PreparedSimulationRuntime>, String> {
        let UpgradeSimulationTarget::Official { target, .. } = target else {
            return Ok(None);
        };

        let canonical_name = target.canonical_runtime_name()?;
        let releases = self
            .runtime_service()
            .official_openclaw_releases(target.version.as_deref(), target.channel.as_deref())?;
        let selected = releases
            .into_iter()
            .next()
            .ok_or_else(|| "OpenClaw release was not found".to_string())?;

        if let Ok(existing) = get_runtime(&canonical_name, &self.env, &self.cwd) {
            let healthy = runtime_integrity_issue(&existing, &self.env).is_none();
            let same_release = existing.release_version.as_deref()
                == Some(selected.version.as_str())
                && existing.source_url.as_deref() == Some(selected.tarball_url.as_str());
            if healthy && same_release {
                return Ok(Some(PreparedSimulationRuntime {
                    name: canonical_name.clone(),
                    note: format!("using installed runtime {canonical_name}"),
                    temporary: false,
                }));
            }
        }

        let runtime_name = simulation_runtime_name(source_name);
        install_runtime_from_selected_official_openclaw_release(
            runtime_name.clone(),
            false,
            official_openclaw_releases_url(&self.env),
            selected,
            RuntimeReleaseDetails::with_selector(
                if target.version.is_some() {
                    Some(RuntimeReleaseSelectorKind::Version)
                } else {
                    Some(RuntimeReleaseSelectorKind::Channel)
                },
                target.version.clone().or_else(|| target.channel.clone()),
            ),
            Some("Temporary runtime for ocm upgrade simulation".to_string()),
            InstallContext {
                env: &self.env,
                cwd: &self.cwd,
            },
        )?;
        Ok(Some(PreparedSimulationRuntime {
            name: runtime_name,
            note: "installed temporary runtime for simulation".to_string(),
            temporary: true,
        }))
    }

    fn ensure_simulation_dev_dependencies(&self, meta: &crate::env::EnvMeta) -> Result<(), String> {
        let dev = meta
            .dev
            .as_ref()
            .ok_or_else(|| format!("environment \"{}\" is missing its dev binding", meta.name))?;
        let worktree_root = Path::new(&dev.worktree_root);
        let pnpm_store = worktree_root.join("node_modules").join(".pnpm");
        let tsx_bin = worktree_root.join("node_modules").join(".bin").join("tsx");
        if pnpm_store.exists() && tsx_bin.exists() {
            return Ok(());
        }

        let output = Command::new("pnpm")
            .arg("install")
            .env_clear()
            .envs(build_openclaw_env(meta, &self.env))
            .current_dir(worktree_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("failed to run pnpm install: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "pnpm install failed: {}",
            summarize_command_output(&output.stdout, &output.stderr)
        ))
    }

    fn run_simulation_check(
        &self,
        simulation_name: &str,
        name: &str,
        args: &[&str],
    ) -> UpgradeSimulationCheck {
        self.run_simulation_check_with_env(simulation_name, name, args, &[])
    }

    fn run_simulation_check_with_env(
        &self,
        simulation_name: &str,
        name: &str,
        args: &[&str],
        extra_env: &[(&str, &str)],
    ) -> UpgradeSimulationCheck {
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        match self
            .environment_service()
            .resolve(simulation_name, None, None, &args)
        {
            Ok(resolved) => match self.run_resolved_for_simulation(resolved, extra_env) {
                Ok(output) if output.status.success() => {
                    UpgradeSimulationCheck::passed(name, output.first_line())
                }
                Ok(output) => UpgradeSimulationCheck::failed(name, output.failure_summary()),
                Err(error) => UpgradeSimulationCheck::failed(name, error),
            },
            Err(error) => UpgradeSimulationCheck::failed(name, error),
        }
    }

    fn run_local_repo_script_check(
        &self,
        simulation_name: &str,
        name: &str,
        script: &str,
    ) -> UpgradeSimulationCheck {
        match self.environment_service().get(simulation_name) {
            Ok(env_meta) => {
                let Some(dev) = env_meta.dev.as_ref() else {
                    return UpgradeSimulationCheck::failed(
                        name,
                        format!(
                            "environment \"{}\" is missing its dev binding",
                            env_meta.name
                        ),
                    );
                };
                let mut command = Command::new("pnpm");
                command
                    .arg(script)
                    .current_dir(&dev.worktree_root)
                    .env_clear()
                    .envs(build_openclaw_env(&env_meta, &self.env))
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                match command.output() {
                    Ok(output) if output.status.success() => UpgradeSimulationCheck::passed(
                        name,
                        SimulationCommandOutput::from_output(output).first_line(),
                    ),
                    Ok(output) => UpgradeSimulationCheck::failed(
                        name,
                        SimulationCommandOutput::from_output(output).failure_summary(),
                    ),
                    Err(error) => UpgradeSimulationCheck::failed(
                        name,
                        format!("failed to run simulation check: {error}"),
                    ),
                }
            }
            Err(error) => UpgradeSimulationCheck::failed(name, error),
        }
    }

    fn run_resolved_for_simulation(
        &self,
        resolved: crate::env::ResolvedExecution,
        extra_env: &[(&str, &str)],
    ) -> Result<SimulationCommandOutput, String> {
        let (mut command, env_meta, source_root, path_prepend) = match resolved {
            crate::env::ResolvedExecution::Launcher {
                env,
                command,
                run_dir,
                ..
            } => {
                let mut process = shell_command(&command);
                process.current_dir(run_dir);
                (process, env, None, None)
            }
            crate::env::ResolvedExecution::Runtime {
                env,
                program,
                program_args,
                path_prepend,
                run_dir,
                ..
            } => {
                let mut process = Command::new(program);
                process.args(program_args).current_dir(run_dir);
                (process, env, None, path_prepend)
            }
            crate::env::ResolvedExecution::Dev {
                env,
                worktree_root,
                program,
                program_args,
                run_dir,
                ..
            } => {
                let mut process = Command::new(program);
                process.args(program_args).current_dir(run_dir);
                (process, env, Some(PathBuf::from(worktree_root)), None)
            }
            crate::env::ResolvedExecution::SourceWatch {
                env,
                source,
                program,
                program_args,
                run_dir,
                ..
            } => {
                let mut process = Command::new(program);
                process.args(program_args).current_dir(run_dir);
                (process, env, Some(PathBuf::from(source.repo_root)), None)
            }
        };
        let mut process_env = match source_root {
            Some(source_root) => build_openclaw_dev_source_env(&env_meta, &self.env, &source_root),
            None => build_openclaw_env(&env_meta, &self.env),
        };
        crate::managed_node::apply_path_prepend_to_environment(
            &mut process_env,
            path_prepend.as_deref(),
        )?;
        for (key, value) in extra_env {
            process_env.insert((*key).to_string(), (*value).to_string());
        }
        let output = command
            .env_clear()
            .envs(process_env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("failed to run simulation check: {error}"))?;
        Ok(SimulationCommandOutput::from_output(output))
    }

    #[allow(clippy::too_many_arguments)]
    fn build_simulation_summary(
        &self,
        source_name: &str,
        simulation_name: &str,
        from_binding_kind: String,
        from_binding_name: String,
        to_binding_kind: String,
        to_binding_name: String,
        scenario: UpgradeSimulationScenario,
        to: String,
        checks: Vec<UpgradeSimulationCheck>,
    ) -> UpgradeSimulationSummary {
        let failed = checks.iter().any(|check| check.status == "failed");
        UpgradeSimulationSummary {
            scenario: scenario.id().to_string(),
            source_env: source_name.to_string(),
            simulation_env: simulation_name.to_string(),
            from_binding_kind,
            from_binding_name,
            to_binding_kind,
            to_binding_name,
            to,
            outcome: if failed { "failed" } else { "passed" }.to_string(),
            cleanup_command: format!(
                "{} env destroy {} --yes",
                self.command_example(),
                simulation_name
            ),
            cleanup: "pending".to_string(),
            note: None,
            checks,
        }
    }

    fn finish_simulation_summary(
        &self,
        mut summary: UpgradeSimulationSummary,
        options: UpgradeSimulationOptions,
    ) -> Result<UpgradeSimulationSummary, String> {
        if options.keep_envs {
            summary.cleanup = "kept".to_string();
            summary.note = Some(
                "simulation artifacts retained because --keep-simulations was set".to_string(),
            );
            return Ok(summary);
        }

        match self
            .environment_service()
            .remove(&summary.simulation_env, true)
        {
            Ok(_) => {
                summary.cleanup = "cleaned".to_string();
            }
            Err(error) => {
                summary.cleanup = "failed".to_string();
                summary.checks.push(UpgradeSimulationCheck::failed(
                    "cleanup simulation env",
                    error,
                ));
                summary.outcome = "failed".to_string();
            }
        }
        Ok(summary)
    }

    fn finish_shared_simulation_runtime(
        &self,
        summaries: &mut [UpgradeSimulationSummary],
        prepared_runtime: Option<&PreparedSimulationRuntime>,
        options: UpgradeSimulationOptions,
    ) -> Result<(), String> {
        let Some(prepared_runtime) = prepared_runtime else {
            return Ok(());
        };
        if options.keep_envs || !prepared_runtime.temporary {
            return Ok(());
        }

        match self.remove_runtime_created_during_upgrade(&prepared_runtime.name) {
            Ok(()) => Ok(()),
            Err(error) => {
                for summary in summaries
                    .iter_mut()
                    .filter(|summary| summary.to_binding_name == prepared_runtime.name)
                {
                    summary.cleanup = "failed".to_string();
                    summary.checks.push(UpgradeSimulationCheck::failed(
                        "cleanup simulation runtime",
                        error.clone(),
                    ));
                    summary.outcome = "failed".to_string();
                }
                Ok(())
            }
        }
    }

    fn upgrade_env(
        &self,
        name: &str,
        target: &UpgradeTarget,
        options: UpgradeOptions,
    ) -> Result<UpgradeEnvSummary, String> {
        let _transaction_lock = lock_upgrade_transaction(name, &self.env, &self.cwd)?;
        let _operation_lock = self.environment_service().lock_operation(name)?;
        self.upgrade_env_locked(name, target, options)
    }

    fn upgrade_env_locked(
        &self,
        name: &str,
        target: &UpgradeTarget,
        options: UpgradeOptions,
    ) -> Result<UpgradeEnvSummary, String> {
        let env = self.environment_service().get(name)?;

        if let Some(runtime_name) = env.default_runtime.as_deref() {
            return self.upgrade_runtime_bound_env(name, runtime_name, target, options);
        }

        if let Some(launcher_name) = env.default_launcher.as_deref() {
            return self.upgrade_launcher_bound_env(name, launcher_name, target, options);
        }

        Err(format!(
            "env \"{name}\" does not have a runtime or launcher binding; use start or env set-runtime/set-launcher first"
        ))
    }

    fn upgrade_runtime_bound_env(
        &self,
        env_name: &str,
        runtime_name: &str,
        target: &UpgradeTarget,
        options: UpgradeOptions,
    ) -> Result<UpgradeEnvSummary, String> {
        let current = self.runtime_service().show(runtime_name)?;
        let previous_binding_name = current.name.clone();

        if target.is_explicit() {
            let resolved = self.resolve_upgrade_target(target)?;
            let target_runtime_name = resolved.name.clone();
            let target_version = self.resolved_target_version(env_name, &resolved)?;
            let source_version = self.ensure_upgrade_is_not_downgrade(
                env_name,
                current.release_version.as_deref(),
                target_version.as_deref(),
            )?;
            if !target.is_named_runtime() {
                self.ensure_runtime_upgrade_isolated(env_name, &target_runtime_name)?;
            }
            let service = self.upgrade_service_status(env_name)?;
            if options.dry_run {
                let binding_changed = target_runtime_name != current.name;
                return Ok(UpgradeEnvSummary {
                    env_name: env_name.to_string(),
                    previous_binding_kind: "runtime".to_string(),
                    previous_binding_name,
                    binding_kind: "runtime".to_string(),
                    binding_name: target_runtime_name,
                    outcome: if binding_changed {
                        "would-switch".to_string()
                    } else {
                        "would-update".to_string()
                    },
                    runtime_release_version: target_version.clone(),
                    runtime_release_channel: resolved.release_channel.clone(),
                    service_action: service_action_for_dry_run(
                        service.as_ref(),
                        binding_changed,
                        true,
                    ),
                    snapshot_id: None,
                    rollback: None,
                    note: Some(
                        "dry run: no runtime, env, service, or snapshot changed".to_string(),
                    ),
                });
            }
            let runtime_names = if target.is_named_runtime() {
                Vec::new()
            } else {
                vec![target_runtime_name.clone()]
            };
            let prepared = match self.prepare_isolated_upgrade_target(env_name, target, resolved) {
                Ok(prepared) => prepared,
                Err(error) => {
                    return self.fail_upgrade_before_cutover(
                        env_name,
                        "runtime",
                        previous_binding_name,
                        "runtime",
                        target_runtime_name,
                        target_version.clone(),
                        target.release_channel_hint(),
                        error,
                    );
                }
            };
            let target_changed = !matches!(prepared.action, OfficialRuntimePrepareAction::Reused);
            let mut transaction = self.begin_upgrade_transaction_locked(
                env_name,
                UpgradeTransactionPlan {
                    source: UpgradeHistoryBinding {
                        kind: "runtime".to_string(),
                        name: current.name.clone(),
                        openclaw_version: source_version,
                    },
                    target: UpgradeHistoryBinding {
                        kind: "runtime".to_string(),
                        name: target_runtime_name.clone(),
                        openclaw_version: target_version.clone(),
                    },
                },
                &runtime_names,
                options.rollback_enabled,
                "pre-upgrade",
                None,
            )?;
            if target_changed {
                transaction.mark_runtime_mutated(&prepared.name);
            }
            let prepared = match prepared.commit() {
                Ok(prepared) => prepared,
                Err(error) => {
                    return self.rollback_failed_upgrade(
                        env_name,
                        "runtime",
                        previous_binding_name,
                        "runtime",
                        target_runtime_name,
                        target_version.clone(),
                        target.release_channel_hint(),
                        transaction,
                        error,
                    );
                }
            };
            let binding_changed = prepared.name != current.name;
            let post_update_note = match self.run_post_core_update(env_name, &prepared.name) {
                Ok(note) => {
                    transaction.mark_post_update_completed(note.as_deref());
                    note
                }
                Err(error) => {
                    transaction.mark_post_update_failed(&error);
                    return self.rollback_failed_upgrade(
                        env_name,
                        "runtime",
                        previous_binding_name,
                        "runtime",
                        prepared.name,
                        prepared.meta.release_version,
                        prepared.meta.release_channel,
                        transaction,
                        error,
                    );
                }
            };
            let publish_result = if binding_changed {
                self.environment_service()
                    .set_runtime_locked(env_name, prepared.name.as_str())
                    .map(|_| ())
            } else {
                self.runtime_service()
                    .refresh_supervisor_if_present(&prepared.name)
            };
            if let Err(error) = publish_result {
                return self.rollback_failed_upgrade(
                    env_name,
                    "runtime",
                    previous_binding_name,
                    "runtime",
                    prepared.name,
                    prepared.meta.release_version,
                    prepared.meta.release_channel,
                    transaction,
                    format!("failed to publish upgraded runtime: {error}"),
                );
            }
            let service_result = self.reconcile_upgraded_service_locked(
                env_name,
                service.as_ref(),
                binding_changed,
                true,
            );
            let (service_action, service_note) = match service_result {
                Ok(result) => result,
                Err(error) => {
                    return self.rollback_failed_upgrade(
                        env_name,
                        "runtime",
                        previous_binding_name,
                        "runtime",
                        prepared.name,
                        prepared.meta.release_version,
                        prepared.meta.release_channel,
                        transaction,
                        error,
                    );
                }
            };
            let runtime_note = service_note.or_else(|| {
                if binding_changed {
                    Some(format!("env now uses runtime {}", prepared.name))
                } else {
                    note_for_official_prepare_action(&prepared.action)
                }
            });
            let verification_note = match self.verify_upgraded_openclaw(
                env_name,
                prepared.meta.release_version.as_deref(),
                service_action.is_some(),
            ) {
                Ok(note) => note,
                Err(error) => {
                    return self.rollback_failed_upgrade(
                        env_name,
                        "runtime",
                        previous_binding_name,
                        "runtime",
                        prepared.name,
                        prepared.meta.release_version,
                        prepared.meta.release_channel,
                        transaction,
                        error,
                    );
                }
            };
            let note = join_optional_warnings(
                join_optional_warnings(post_update_note, runtime_note),
                verification_note,
            );

            let summary = UpgradeEnvSummary {
                env_name: env_name.to_string(),
                previous_binding_kind: "runtime".to_string(),
                previous_binding_name,
                binding_kind: "runtime".to_string(),
                binding_name: prepared.name.clone(),
                outcome: if binding_changed {
                    "switched".to_string()
                } else {
                    outcome_for_official_prepare_action(&prepared.action)
                },
                runtime_release_version: prepared.meta.release_version.clone(),
                runtime_release_channel: prepared.meta.release_channel.clone(),
                service_action,
                snapshot_id: Some(transaction.snapshot_id.clone()),
                rollback: None,
                note,
            };
            return self.finish_successful_upgrade(summary, transaction);
        }

        if current.source_manifest_url.is_none() {
            return Ok(UpgradeEnvSummary {
                env_name: env_name.to_string(),
                previous_binding_kind: "runtime".to_string(),
                previous_binding_name: previous_binding_name.clone(),
                binding_kind: "runtime".to_string(),
                binding_name: previous_binding_name,
                outcome: "manual-runtime".to_string(),
                runtime_release_version: current.release_version.clone(),
                runtime_release_channel: current.release_channel.clone(),
                service_action: None,
                snapshot_id: None,
                rollback: None,
                note: Some(
                    "this env uses a manual runtime; update it outside ocm or switch to a published release"
                        .to_string(),
                ),
            });
        }

        if current.release_selector_kind == Some(RuntimeReleaseSelectorKind::Version) {
            return Ok(UpgradeEnvSummary {
                env_name: env_name.to_string(),
                previous_binding_kind: "runtime".to_string(),
                previous_binding_name: previous_binding_name.clone(),
                binding_kind: "runtime".to_string(),
                binding_name: previous_binding_name,
                outcome: "pinned".to_string(),
                runtime_release_version: current.release_version.clone(),
                runtime_release_channel: current.release_channel.clone(),
                service_action: None,
                snapshot_id: None,
                rollback: None,
                note: Some(
                    "this env is pinned to an exact release; pass --version or --channel to move it"
                        .to_string(),
                ),
            });
        }

        self.ensure_runtime_upgrade_isolated(env_name, &current.name)?;

        if is_official_openclaw_releases_url(current.source_manifest_url.as_deref(), &self.env) {
            let service = self.upgrade_service_status(env_name)?;
            let target = UpgradeTarget {
                version: None,
                channel: current.release_selector_value.clone(),
                runtime: None,
            };
            let resolved = self.resolve_upgrade_target(&target)?;
            let target_runtime_name = resolved.name.clone();
            let target_version = self.resolved_target_version(env_name, &resolved)?;
            let source_version = self.ensure_upgrade_is_not_downgrade(
                env_name,
                current.release_version.as_deref(),
                target_version.as_deref(),
            )?;
            if options.dry_run {
                return Ok(UpgradeEnvSummary {
                    env_name: env_name.to_string(),
                    previous_binding_kind: "runtime".to_string(),
                    previous_binding_name: previous_binding_name.clone(),
                    binding_kind: "runtime".to_string(),
                    binding_name: target_runtime_name,
                    outcome: "would-update".to_string(),
                    runtime_release_version: target_version.clone(),
                    runtime_release_channel: resolved.release_channel.clone(),
                    service_action: service_action_for_dry_run(service.as_ref(), false, true),
                    snapshot_id: None,
                    rollback: None,
                    note: Some(
                        "dry run: no runtime, env, service, or snapshot changed".to_string(),
                    ),
                });
            }
            let prepared = match self.prepare_isolated_upgrade_target(env_name, &target, resolved) {
                Ok(prepared) => prepared,
                Err(error) => {
                    return self.fail_upgrade_before_cutover(
                        env_name,
                        "runtime",
                        previous_binding_name,
                        "runtime",
                        target_runtime_name,
                        target_version.clone(),
                        target.release_channel_hint(),
                        error,
                    );
                }
            };
            let changed = matches!(
                prepared.action,
                OfficialRuntimePrepareAction::Installed | OfficialRuntimePrepareAction::Updated
            );
            let mut transaction = self.begin_upgrade_transaction_locked(
                env_name,
                UpgradeTransactionPlan {
                    source: UpgradeHistoryBinding {
                        kind: "runtime".to_string(),
                        name: current.name.clone(),
                        openclaw_version: source_version,
                    },
                    target: UpgradeHistoryBinding {
                        kind: "runtime".to_string(),
                        name: target_runtime_name.clone(),
                        openclaw_version: target_version.clone(),
                    },
                },
                std::slice::from_ref(&target_runtime_name),
                options.rollback_enabled,
                "pre-upgrade",
                None,
            )?;
            if changed {
                transaction.mark_runtime_mutated(&prepared.name);
            }
            let prepared = match prepared.commit() {
                Ok(prepared) => prepared,
                Err(error) => {
                    return self.rollback_failed_upgrade(
                        env_name,
                        "runtime",
                        previous_binding_name,
                        "runtime",
                        target_runtime_name,
                        target_version.clone(),
                        target.release_channel_hint(),
                        transaction,
                        error,
                    );
                }
            };
            let post_update_note = if changed {
                match self.run_post_core_update(env_name, &prepared.name) {
                    Ok(note) => {
                        transaction.mark_post_update_completed(note.as_deref());
                        note
                    }
                    Err(error) => {
                        transaction.mark_post_update_failed(&error);
                        return self.rollback_failed_upgrade(
                            env_name,
                            "runtime",
                            previous_binding_name,
                            "runtime",
                            prepared.name,
                            prepared.meta.release_version,
                            prepared.meta.release_channel,
                            transaction,
                            error,
                        );
                    }
                }
            } else {
                transaction.mark_post_update_not_needed();
                None
            };
            if changed
                && let Err(error) = self
                    .runtime_service()
                    .refresh_supervisor_if_present(&prepared.name)
            {
                return self.rollback_failed_upgrade(
                    env_name,
                    "runtime",
                    previous_binding_name,
                    "runtime",
                    prepared.name,
                    prepared.meta.release_version,
                    prepared.meta.release_channel,
                    transaction,
                    format!("failed to publish upgraded runtime: {error}"),
                );
            }
            let service_result = self.reconcile_upgraded_service_locked(
                env_name,
                service.as_ref(),
                false,
                changed || transaction.service_quiesced,
            );
            let (service_action, service_note) = match service_result {
                Ok(result) => result,
                Err(error) => {
                    return self.rollback_failed_upgrade(
                        env_name,
                        "runtime",
                        previous_binding_name,
                        "runtime",
                        prepared.name,
                        prepared.meta.release_version,
                        prepared.meta.release_channel,
                        transaction,
                        error,
                    );
                }
            };
            let verification_note = match self.verify_upgraded_openclaw(
                env_name,
                prepared.meta.release_version.as_deref(),
                service_action.is_some(),
            ) {
                Ok(note) => note,
                Err(error) => {
                    return self.rollback_failed_upgrade(
                        env_name,
                        "runtime",
                        previous_binding_name,
                        "runtime",
                        prepared.name,
                        prepared.meta.release_version,
                        prepared.meta.release_channel,
                        transaction,
                        error,
                    );
                }
            };
            let summary = UpgradeEnvSummary {
                env_name: env_name.to_string(),
                previous_binding_kind: "runtime".to_string(),
                previous_binding_name: previous_binding_name.clone(),
                binding_kind: "runtime".to_string(),
                binding_name: prepared.name.clone(),
                outcome: outcome_for_official_prepare_action(&prepared.action),
                runtime_release_version: prepared.meta.release_version.clone(),
                runtime_release_channel: prepared.meta.release_channel.clone(),
                service_action,
                snapshot_id: Some(transaction.snapshot_id.clone()),
                rollback: None,
                note: join_optional_warnings(
                    join_optional_warnings(
                        post_update_note,
                        service_note.or_else(|| note_for_official_prepare_action(&prepared.action)),
                    ),
                    verification_note,
                ),
            };
            return self.finish_successful_upgrade(summary, transaction);
        }

        let resolved_update = self.runtime_service().resolve_update_from_release(
            crate::runtime::UpdateRuntimeFromReleaseOptions {
                name: current.name.clone(),
                version: None,
                channel: None,
            },
        )?;
        let target_version = resolved_update.release_version().to_string();
        let source_version = self.ensure_upgrade_is_not_downgrade(
            env_name,
            current.release_version.as_deref(),
            Some(&target_version),
        )?;
        let service = self.upgrade_service_status(env_name)?;
        if options.dry_run {
            return Ok(UpgradeEnvSummary {
                env_name: env_name.to_string(),
                previous_binding_kind: "runtime".to_string(),
                previous_binding_name: previous_binding_name.clone(),
                binding_kind: "runtime".to_string(),
                binding_name: previous_binding_name,
                outcome: "would-update".to_string(),
                runtime_release_version: Some(target_version),
                runtime_release_channel: current.release_channel.clone(),
                service_action: service_action_for_dry_run(service.as_ref(), false, true),
                snapshot_id: None,
                rollback: None,
                note: Some("dry run: no runtime, env, service, or snapshot changed".to_string()),
            });
        }
        let prepared =
            match self.with_progress(format!("Updating runtime {}", current.name), || {
                self.with_isolated_runtime_mutation(env_name, &current.name, || {
                    self.runtime_service()
                        .prepare_resolved_update(resolved_update)
                })
            }) {
                Ok(prepared) => prepared,
                Err(error) => {
                    return self.fail_upgrade_before_cutover(
                        env_name,
                        "runtime",
                        previous_binding_name,
                        "runtime",
                        current.name,
                        current.release_version,
                        current.release_channel,
                        error,
                    );
                }
            };
        let mut transaction = self.begin_upgrade_transaction_locked(
            env_name,
            UpgradeTransactionPlan {
                source: UpgradeHistoryBinding {
                    kind: "runtime".to_string(),
                    name: current.name.clone(),
                    openclaw_version: source_version,
                },
                target: UpgradeHistoryBinding {
                    kind: "runtime".to_string(),
                    name: current.name.clone(),
                    openclaw_version: Some(target_version.clone()),
                },
            },
            std::slice::from_ref(&current.name),
            options.rollback_enabled,
            "pre-upgrade",
            None,
        )?;
        transaction.mark_runtime_mutated(&current.name);
        let updated = match prepared.commit() {
            Ok(updated) => updated,
            Err(error) => {
                return self.rollback_failed_upgrade(
                    env_name,
                    "runtime",
                    previous_binding_name,
                    "runtime",
                    current.name,
                    current.release_version,
                    current.release_channel,
                    transaction,
                    error,
                );
            }
        };
        let post_update_note = match self.run_post_core_update(env_name, &updated.name) {
            Ok(note) => {
                transaction.mark_post_update_completed(note.as_deref());
                note
            }
            Err(error) => {
                transaction.mark_post_update_failed(&error);
                return self.rollback_failed_upgrade(
                    env_name,
                    "runtime",
                    previous_binding_name,
                    "runtime",
                    updated.name,
                    updated.release_version,
                    updated.release_channel,
                    transaction,
                    error,
                );
            }
        };
        if let Err(error) = self
            .runtime_service()
            .refresh_supervisor_if_present(&updated.name)
        {
            return self.rollback_failed_upgrade(
                env_name,
                "runtime",
                previous_binding_name,
                "runtime",
                updated.name,
                updated.release_version,
                updated.release_channel,
                transaction,
                format!("failed to publish upgraded runtime: {error}"),
            );
        }
        let service_result =
            self.reconcile_upgraded_service_locked(env_name, service.as_ref(), false, true);
        let (service_action, service_note) = match service_result {
            Ok(result) => result,
            Err(error) => {
                return self.rollback_failed_upgrade(
                    env_name,
                    "runtime",
                    previous_binding_name,
                    "runtime",
                    updated.name,
                    updated.release_version,
                    updated.release_channel,
                    transaction,
                    error,
                );
            }
        };
        let verification_note = match self.verify_upgraded_openclaw(
            env_name,
            updated.release_version.as_deref(),
            service_action.is_some(),
        ) {
            Ok(note) => note,
            Err(error) => {
                return self.rollback_failed_upgrade(
                    env_name,
                    "runtime",
                    previous_binding_name,
                    "runtime",
                    updated.name,
                    updated.release_version,
                    updated.release_channel,
                    transaction,
                    error,
                );
            }
        };
        let summary = UpgradeEnvSummary {
            env_name: env_name.to_string(),
            previous_binding_kind: "runtime".to_string(),
            previous_binding_name: previous_binding_name.clone(),
            binding_kind: "runtime".to_string(),
            binding_name: updated.name.clone(),
            outcome: "updated".to_string(),
            runtime_release_version: updated.release_version.clone(),
            runtime_release_channel: updated.release_channel.clone(),
            service_action,
            snapshot_id: Some(transaction.snapshot_id.clone()),
            rollback: None,
            note: join_optional_warnings(
                join_optional_warnings(post_update_note, service_note),
                verification_note,
            ),
        };
        self.finish_successful_upgrade(summary, transaction)
    }

    fn ensure_runtime_upgrade_isolated(
        &self,
        env_name: &str,
        runtime_name: &str,
    ) -> Result<(), String> {
        let mut siblings = self
            .environment_service()
            .list()?
            .into_iter()
            .filter(|env| {
                env.name != env_name && env.default_runtime.as_deref() == Some(runtime_name)
            })
            .map(|env| env.name)
            .collect::<Vec<_>>();
        if siblings.is_empty() {
            return Ok(());
        }

        siblings.sort();
        let siblings = siblings
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "runtime \"{runtime_name}\" is shared with {siblings}; upgrading env \"{env_name}\" would replace their OpenClaw runtime before their config and state are migrated. Use --runtime {runtime_name} to reuse the installed runtime without updating it, or --version <version> to move this env to an isolated exact release"
        ))
    }

    fn upgrade_launcher_bound_env(
        &self,
        env_name: &str,
        launcher_name: &str,
        target: &UpgradeTarget,
        options: UpgradeOptions,
    ) -> Result<UpgradeEnvSummary, String> {
        if !target.is_explicit() {
            return Ok(UpgradeEnvSummary {
                env_name: env_name.to_string(),
                previous_binding_kind: "launcher".to_string(),
                previous_binding_name: launcher_name.to_string(),
                binding_kind: "launcher".to_string(),
                binding_name: launcher_name.to_string(),
                outcome: "local-command".to_string(),
                runtime_release_version: None,
                runtime_release_channel: None,
                service_action: None,
                snapshot_id: None,
                rollback: None,
                note: Some(
                    "this env uses a local command; update that checkout or command outside ocm"
                        .to_string(),
                ),
            });
        }

        let resolved = self.resolve_upgrade_target(target)?;
        let target_runtime_name = resolved.name.clone();
        let target_version = self.resolved_target_version(env_name, &resolved)?;
        let source_version =
            self.ensure_upgrade_is_not_downgrade(env_name, None, target_version.as_deref())?;
        if !target.is_named_runtime() {
            self.ensure_runtime_upgrade_isolated(env_name, &target_runtime_name)?;
        }
        let service = self.upgrade_service_status(env_name)?;
        if options.dry_run {
            return Ok(UpgradeEnvSummary {
                env_name: env_name.to_string(),
                previous_binding_kind: "launcher".to_string(),
                previous_binding_name: launcher_name.to_string(),
                binding_kind: "runtime".to_string(),
                binding_name: target_runtime_name,
                outcome: "would-switch".to_string(),
                runtime_release_version: target_version.clone(),
                runtime_release_channel: resolved.release_channel.clone(),
                service_action: service_action_for_dry_run(service.as_ref(), true, true),
                snapshot_id: None,
                rollback: None,
                note: Some("dry run: no runtime, env, service, or snapshot changed".to_string()),
            });
        }

        let prepared = match self.prepare_isolated_upgrade_target(env_name, target, resolved) {
            Ok(prepared) => prepared,
            Err(error) => {
                return self.fail_upgrade_before_cutover(
                    env_name,
                    "launcher",
                    launcher_name.to_string(),
                    "runtime",
                    target_runtime_name,
                    target_version.clone(),
                    target.release_channel_hint(),
                    error,
                );
            }
        };
        let target_changed = !matches!(prepared.action, OfficialRuntimePrepareAction::Reused);
        let mut transaction = self.begin_upgrade_transaction_locked(
            env_name,
            UpgradeTransactionPlan {
                source: UpgradeHistoryBinding {
                    kind: "launcher".to_string(),
                    name: launcher_name.to_string(),
                    openclaw_version: source_version,
                },
                target: UpgradeHistoryBinding {
                    kind: "runtime".to_string(),
                    name: target_runtime_name.clone(),
                    openclaw_version: target_version.clone(),
                },
            },
            std::slice::from_ref(&target_runtime_name),
            options.rollback_enabled,
            "pre-upgrade",
            None,
        )?;
        if target_changed {
            transaction.mark_runtime_mutated(&prepared.name);
        }
        let prepared = match prepared.commit() {
            Ok(prepared) => prepared,
            Err(error) => {
                return self.rollback_failed_upgrade(
                    env_name,
                    "launcher",
                    launcher_name.to_string(),
                    "runtime",
                    target_runtime_name,
                    target_version.clone(),
                    target.release_channel_hint(),
                    transaction,
                    error,
                );
            }
        };
        let post_update_note = match self.run_post_core_update(env_name, &prepared.name) {
            Ok(note) => {
                transaction.mark_post_update_completed(note.as_deref());
                note
            }
            Err(error) => {
                transaction.mark_post_update_failed(&error);
                return self.rollback_failed_upgrade(
                    env_name,
                    "launcher",
                    launcher_name.to_string(),
                    "runtime",
                    prepared.name,
                    prepared.meta.release_version,
                    prepared.meta.release_channel,
                    transaction,
                    error,
                );
            }
        };
        if let Err(error) = self
            .environment_service()
            .set_runtime_locked(env_name, prepared.name.as_str())
        {
            return self.rollback_failed_upgrade(
                env_name,
                "launcher",
                launcher_name.to_string(),
                "runtime",
                prepared.name,
                prepared.meta.release_version,
                prepared.meta.release_channel,
                transaction,
                format!("failed to publish upgraded runtime: {error}"),
            );
        }
        let service_result =
            self.reconcile_upgraded_service_locked(env_name, service.as_ref(), true, true);
        let (service_action, service_note) = match service_result {
            Ok(result) => result,
            Err(error) => {
                return self.rollback_failed_upgrade(
                    env_name,
                    "launcher",
                    launcher_name.to_string(),
                    "runtime",
                    prepared.name,
                    prepared.meta.release_version,
                    prepared.meta.release_channel,
                    transaction,
                    error,
                );
            }
        };
        let verification_note = match self.verify_upgraded_openclaw(
            env_name,
            prepared.meta.release_version.as_deref(),
            service_action.is_some(),
        ) {
            Ok(note) => note,
            Err(error) => {
                return self.rollback_failed_upgrade(
                    env_name,
                    "launcher",
                    launcher_name.to_string(),
                    "runtime",
                    prepared.name,
                    prepared.meta.release_version,
                    prepared.meta.release_channel,
                    transaction,
                    error,
                );
            }
        };
        let summary = UpgradeEnvSummary {
            env_name: env_name.to_string(),
            previous_binding_kind: "launcher".to_string(),
            previous_binding_name: launcher_name.to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: prepared.name.clone(),
            outcome: "switched".to_string(),
            runtime_release_version: prepared.meta.release_version.clone(),
            runtime_release_channel: prepared.meta.release_channel.clone(),
            service_action,
            snapshot_id: Some(transaction.snapshot_id.clone()),
            rollback: None,
            note: join_optional_warnings(
                join_optional_warnings(
                    post_update_note,
                    service_note
                        .or_else(|| Some(format!("env now uses runtime {}", prepared.name))),
                ),
                verification_note,
            ),
        };
        self.finish_successful_upgrade(summary, transaction)
    }

    fn upgrade_service_status(&self, env_name: &str) -> Result<Option<ServiceSummary>, String> {
        let meta = self.environment_service().get(env_name)?;
        if !meta.service_enabled || !meta.service_running {
            return Ok(None);
        }
        self.service_service().status(env_name).map(Some)
    }

    fn resolved_target_version(
        &self,
        env_name: &str,
        target: &ResolvedUpgradeTarget,
    ) -> Result<Option<String>, String> {
        let version_hint = target
            .release_version
            .as_deref()
            .filter(|version| compare_runtime_release_versions(version, version).is_some());
        if matches!(&target.kind, ResolvedUpgradeTargetKind::Official(_)) {
            return Ok(version_hint.map(str::to_string));
        }

        let Ok(output) = self.run_update_mode_openclaw_command_output(
            env_name,
            &target.name,
            "target openclaw --version",
            &["--version"],
        ) else {
            return Ok(version_hint.map(str::to_string));
        };
        if !output.status.success() {
            return Ok(version_hint.map(str::to_string));
        }
        Ok(
            release_version_from_output(&output.first_line(), version_hint)
                .or_else(|| version_hint.map(str::to_string)),
        )
    }

    fn ensure_upgrade_is_not_downgrade(
        &self,
        env_name: &str,
        current_version_hint: Option<&str>,
        target_version: Option<&str>,
    ) -> Result<Option<String>, String> {
        let current_version_hint = current_version_hint
            .filter(|version| compare_runtime_release_versions(version, version).is_some());
        let current_version =
            match self.run_openclaw_command(env_name, "current openclaw --version", &["--version"])
            {
                Ok(current) => {
                    release_version_from_output(&current.first_line(), current_version_hint)
                        .or_else(|| current_version_hint.map(str::to_string))
                }
                Err(_) => current_version_hint.map(str::to_string),
            };
        let Some(target_version) = target_version else {
            return Ok(current_version);
        };
        let Some(current_version) = current_version else {
            return Ok(None);
        };

        if current_version == target_version {
            return Ok(Some(current_version));
        }

        match compare_runtime_release_versions(&current_version, target_version) {
            Some(std::cmp::Ordering::Greater) => Err(format!(
                "refusing to downgrade env \"{env_name}\" from OpenClaw {current_version} to {target_version}: OCM does not run reverse config or SQLite state migrations, and newer environment state may be unreadable by the older runtime. Restore a complete pre-upgrade snapshot captured with {target_version} instead of switching only the runtime"
            )),
            Some(_) => Ok(Some(current_version)),
            None => Err(format!(
                "cannot safely compare current OpenClaw version \"{current_version}\" with target \"{target_version}\"; refusing to change env \"{env_name}\" without downgrade safety"
            )),
        }
    }

    fn resolve_upgrade_target(
        &self,
        target: &UpgradeTarget,
    ) -> Result<ResolvedUpgradeTarget, String> {
        let runtime_name = target.canonical_runtime_name()?;
        if target.is_named_runtime() {
            let meta = self.runtime_service().show(&runtime_name)?;
            if let Some(issue) = runtime_integrity_issue(&meta, &self.env) {
                return Err(format!(
                    "runtime \"{runtime_name}\" is not healthy: {issue}",
                ));
            }
            return Ok(ResolvedUpgradeTarget {
                name: runtime_name,
                release_version: meta.release_version.clone(),
                release_channel: meta.release_channel.clone(),
                kind: ResolvedUpgradeTargetKind::Named(Box::new(meta)),
            });
        }

        let release = self
            .runtime_service()
            .official_openclaw_releases(target.version.as_deref(), target.channel.as_deref())?
            .into_iter()
            .next()
            .ok_or_else(|| "OpenClaw release was not found".to_string())?;
        Ok(ResolvedUpgradeTarget {
            name: runtime_name,
            release_version: Some(release.version.clone()),
            release_channel: release.channel.clone(),
            kind: ResolvedUpgradeTargetKind::Official(release),
        })
    }

    fn prepare_resolved_upgrade_target(
        &self,
        env_name: &str,
        target: &UpgradeTarget,
        resolved: ResolvedUpgradeTarget,
    ) -> Result<PreparedUpgradeTarget, String> {
        match resolved.kind {
            ResolvedUpgradeTargetKind::Named(meta) => Ok(PreparedUpgradeTarget {
                name: resolved.name,
                prepared_binary_path: PathBuf::from(&meta.binary_path),
                meta: *meta,
                action: OfficialRuntimePrepareAction::Reused,
                staged: None,
            }),
            ResolvedUpgradeTargetKind::Official(release) => {
                let staged = self.with_progress(
                    format!("Preparing OpenClaw runtime for {env_name}"),
                    || {
                        self.runtime_service()
                            .prepare_selected_official_openclaw_runtime_deferred(
                                InstallRuntimeFromOfficialReleaseOptions {
                                    name: resolved.name.clone(),
                                    version: target.version.clone(),
                                    channel: target.channel.clone(),
                                    description: None,
                                    force: false,
                                },
                                release,
                            )
                    },
                )?;
                let prepared_binary_path = staged.prepared_binary_path();
                let meta = staged.meta().clone();
                let action = staged.action();
                Ok(PreparedUpgradeTarget {
                    name: resolved.name,
                    prepared_binary_path,
                    meta,
                    action,
                    staged: Some(staged),
                })
            }
        }
    }

    fn prepare_isolated_upgrade_target(
        &self,
        env_name: &str,
        target: &UpgradeTarget,
        resolved: ResolvedUpgradeTarget,
    ) -> Result<PreparedUpgradeTarget, String> {
        let prepared = if target.is_named_runtime() {
            self.prepare_resolved_upgrade_target(env_name, target, resolved)?
        } else {
            let runtime_name = resolved.name.clone();
            self.with_isolated_runtime_mutation(env_name, &runtime_name, || {
                self.prepare_resolved_upgrade_target(env_name, target, resolved)
            })?
        };
        self.validate_prepared_upgrade_target(env_name, &prepared)?;
        Ok(prepared)
    }

    fn validate_prepared_upgrade_target(
        &self,
        env_name: &str,
        prepared: &PreparedUpgradeTarget,
    ) -> Result<(), String> {
        const CHECK_ID: &str = "codex/managed-app-server";
        let args = [
            "doctor".to_string(),
            "--lint".to_string(),
            "--only".to_string(),
            CHECK_ID.to_string(),
            "--json".to_string(),
        ];
        let mut meta = prepared.meta.clone();
        meta.binary_path = display_path(&prepared.prepared_binary_path);
        let launch = resolve_runtime_launch(&meta, &args, &self.env, &self.cwd, true)
            .map_err(|error| format!("candidate managed Codex preflight failed: {error}"))?;
        let env_meta = self
            .environment_service()
            .get(env_name)
            .map_err(|error| format!("candidate managed Codex preflight failed: {error}"))?;
        let mut process_env = build_openclaw_env(&env_meta, &self.env);
        crate::managed_node::apply_path_prepend_to_environment(
            &mut process_env,
            launch.path_prepend.as_deref(),
        )
        .map_err(|error| format!("candidate managed Codex preflight failed: {error}"))?;
        process_env.insert("OPENCLAW_UPDATE_IN_PROGRESS".to_string(), "1".to_string());

        let output = Command::new(&launch.program)
            .args(&launch.args)
            .current_dir(resolve_runtime_run_dir(&self.cwd))
            .env_clear()
            .envs(process_env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| {
                format!(
                    "candidate managed Codex preflight failed to start {}: {error}",
                    display_path(Path::new(&launch.program))
                )
            })?;
        let output = SimulationCommandOutput::from_output(output);
        if output.status.success() {
            return Ok(());
        }
        if candidate_codex_preflight_is_unsupported(&output.stdout, &output.stderr) {
            return Ok(());
        }
        Err(format!(
            "candidate managed Codex preflight failed: {}. Repair or reinstall the staged OpenClaw runtime, then rerun the upgrade; the source environment was not changed",
            output.failure_summary()
        ))
    }

    fn with_isolated_runtime_mutation<T>(
        &self,
        env_name: &str,
        runtime_name: &str,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _registry_lock = lock_env_registry(&self.env, &self.cwd)?;
        self.ensure_runtime_upgrade_isolated(env_name, runtime_name)?;
        operation()
    }

    fn reconcile_upgraded_service_locked(
        &self,
        env_name: &str,
        service: Option<&ServiceSummary>,
        binding_changed: bool,
        runtime_changed: bool,
    ) -> Result<(Option<String>, Option<String>), String> {
        let Some(service) = service else {
            return Ok((None, None));
        };
        if !service.installed || !service.desired_running {
            return Ok((None, None));
        }
        if !binding_changed && !runtime_changed {
            return Ok((None, None));
        }

        if service.running {
            let restart = self
                .with_progress(format!("Restarting service for {env_name}"), || {
                    self.service_service().restart_locked(env_name)
                })?;
            self.wait_for_restarted_gateway_health(env_name, restart.desired_running)?;
            let note = join_warnings(&restart.warnings);
            return Ok((Some("restarted".to_string()), note));
        }

        if binding_changed || runtime_changed {
            let start = self.with_progress(format!("Starting service for {env_name}"), || {
                self.service_service().start_locked(env_name)
            })?;
            self.wait_for_restarted_gateway_health(env_name, start.desired_running)?;
            let note = join_warnings(&start.warnings);
            return Ok((Some("started".to_string()), note));
        }

        Ok((None, None))
    }

    pub(super) fn wait_for_restarted_gateway_health(
        &self,
        env_name: &str,
        action_desired_running: bool,
    ) -> Result<(), String> {
        if !action_desired_running {
            return Ok(());
        }

        let readiness = wait_for_gateway_readiness(env_name, &self.env, &self.cwd)?;
        if readiness.ready {
            Ok(())
        } else {
            Err(format!(
                "service restart did not recover: {}",
                readiness
                    .issue
                    .unwrap_or_else(|| "gateway did not become ready".to_string())
            ))
        }
    }

    fn verify_upgraded_openclaw(
        &self,
        env_name: &str,
        expected_version: Option<&str>,
        verify_gateway: bool,
    ) -> Result<Option<String>, String> {
        let version = self.run_openclaw_command(env_name, "openclaw --version", &["--version"])?;
        let actual_version = version.first_line();
        if let Some(expected_version) = expected_version
            && !version_output_matches_expected(actual_version.trim(), expected_version)
        {
            return Err(format!(
                "post-upgrade version verification failed: expected {expected_version}, got {}",
                actual_version.trim()
            ));
        }

        if verify_gateway {
            let gateway_status = self.capture_openclaw_command(
                env_name,
                "openclaw gateway status",
                &["gateway", "status", "--deep", "--json"],
            )?;
            if let Err(error) = verify_gateway_status_readiness(&gateway_status.stdout) {
                return if gateway_status.status.success() {
                    Err(error)
                } else {
                    Err(format!("{error}; {}", gateway_status.failure_summary()))
                };
            }
        }

        Ok(Some(format!(
            "post-upgrade verification completed for OpenClaw {}",
            actual_version.trim()
        )))
    }

    fn run_openclaw_command(
        &self,
        env_name: &str,
        name: &str,
        args: &[&str],
    ) -> Result<SimulationCommandOutput, String> {
        let output = self.capture_openclaw_command(env_name, name, args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(format!("{name} failed: {}", output.failure_summary()))
        }
    }

    fn capture_openclaw_command(
        &self,
        env_name: &str,
        name: &str,
        args: &[&str],
    ) -> Result<SimulationCommandOutput, String> {
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let resolved = self
            .environment_service()
            .resolve(env_name, None, None, &args)
            .map_err(|error| format!("{name} failed: {error}"))?;
        self.run_resolved_for_simulation(resolved, &[])
            .map_err(|error| format!("{name} failed: {error}"))
    }

    fn run_post_core_update(
        &self,
        env_name: &str,
        runtime_name: &str,
    ) -> Result<Option<String>, String> {
        // Resolve the replacement explicitly while the previous binding remains published.
        // A failed finalizer can then roll back without ever activating the replacement.
        let config_repaired = self.repair_target_openclaw_config(env_name, runtime_name)?;
        self.run_update_mode_openclaw_command(
            env_name,
            runtime_name,
            "openclaw update finalize",
            &["update", "finalize", "--json", "--yes", "--no-restart"],
        )?;
        Ok(Some(if config_repaired {
            "OpenClaw config repair and update finalization completed".to_string()
        } else {
            "OpenClaw update finalization completed".to_string()
        }))
    }

    fn repair_target_openclaw_config(
        &self,
        env_name: &str,
        runtime_name: &str,
    ) -> Result<bool, String> {
        let env = self
            .environment_service()
            .get(env_name)
            .map_err(|error| format!("failed to inspect OpenClaw config: {error}"))?;
        let config_path = derive_env_paths(Path::new(&env.root)).config_path;
        if !config_path.exists() {
            return Ok(false);
        }

        let validation = self.run_update_mode_openclaw_command_output(
            env_name,
            runtime_name,
            "openclaw config validate",
            &["config", "validate"],
        )?;
        if validation.status.success() {
            return Ok(false);
        }
        if command_output_reports_unsupported_command(&validation.stdout, &validation.stderr) {
            return Ok(false);
        }

        self.run_update_mode_openclaw_command(
            env_name,
            runtime_name,
            "openclaw doctor",
            &["doctor", "--non-interactive", "--fix"],
        )?;
        self.run_update_mode_openclaw_command(
            env_name,
            runtime_name,
            "openclaw config validate after doctor",
            &["config", "validate"],
        )?;
        Ok(true)
    }

    fn run_update_mode_openclaw_command(
        &self,
        env_name: &str,
        runtime_name: &str,
        name: &str,
        args: &[&str],
    ) -> Result<(), String> {
        let output =
            self.run_update_mode_openclaw_command_output(env_name, runtime_name, name, args)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!("{name} failed: {}", output.failure_summary()))
        }
    }

    fn run_update_mode_openclaw_command_output(
        &self,
        env_name: &str,
        runtime_name: &str,
        name: &str,
        args: &[&str],
    ) -> Result<SimulationCommandOutput, String> {
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let resolved = self
            .environment_service()
            .resolve(env_name, Some(runtime_name.to_string()), None, &args)
            .map_err(|error| format!("{name} failed: {error}"))?;
        self.run_resolved_for_simulation(
            resolved,
            &[
                ("OPENCLAW_UPDATE_IN_PROGRESS", "1"),
                ("OPENCLAW_UPDATE_PARENT_SUPPORTS_DOCTOR_CONFIG_WRITE", "1"),
                ("OPENCLAW_UPDATE_PARENT_SUPPORTS_GATEWAY_RESTART", "1"),
                ("OPENCLAW_UPDATE_PARENT_ALLOWS_GATEWAY_SERVICE_REPAIR", "0"),
                ("OPENCLAW_UPDATE_PARENT_ALLOWS_GATEWAY_ACTIVATION", "0"),
            ],
        )
        .map_err(|error| format!("{name} failed: {error}"))
    }

    fn run_launcher_mode_openclaw_command_output(
        &self,
        env_name: &str,
        launcher_name: &str,
        name: &str,
        args: &[&str],
    ) -> Result<SimulationCommandOutput, String> {
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let resolved = self
            .environment_service()
            .resolve(env_name, None, Some(launcher_name.to_string()), &args)
            .map_err(|error| format!("{name} failed: {error}"))?;
        self.run_resolved_for_simulation(resolved, &[])
            .map_err(|error| format!("{name} failed: {error}"))
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_upgrade_transaction_locked(
        &self,
        env_name: &str,
        plan: UpgradeTransactionPlan,
        runtime_names: &[String],
        rollback_enabled: bool,
        snapshot_label: &str,
        rollback_of: Option<String>,
    ) -> Result<UpgradeTransaction, String> {
        let env_meta = self.environment_service().get(env_name)?;
        let prepared = self
            .environment_service()
            .prepare_snapshot_capture_locked(env_name)?;
        let mut seen = BTreeSet::new();
        let mut runtime_backups: Vec<RuntimeRollbackBackup> = Vec::new();
        let mut created_runtime_names = Vec::new();

        for runtime_name in runtime_names {
            if !seen.insert(runtime_name.clone()) {
                continue;
            }
            let meta_path = match runtime_meta_path(runtime_name, &self.env, &self.cwd) {
                Ok(meta_path) => meta_path,
                Err(error) => {
                    for backup in runtime_backups {
                        backup.cleanup();
                    }
                    return Err(error);
                }
            };
            if meta_path.exists() {
                let runtime = match get_runtime(runtime_name, &self.env, &self.cwd) {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        for backup in runtime_backups {
                            backup.cleanup();
                        }
                        return Err(error);
                    }
                };
                match self.backup_runtime_for_upgrade(&runtime) {
                    Ok(backup) => runtime_backups.push(backup),
                    Err(error) => {
                        for backup in runtime_backups {
                            backup.cleanup();
                        }
                        return Err(error);
                    }
                }
            } else {
                created_runtime_names.push(runtime_name.clone());
            }
        }

        let service_state = match self.service_service().quiesce_for_snapshot_locked(env_name) {
            Ok(service_state) => service_state,
            Err(error) => {
                for backup in runtime_backups {
                    backup.cleanup();
                }
                return Err(error);
            }
        };
        let started_at = time::OffsetDateTime::now_utc();
        let id = format!(
            "{}-{:09}",
            started_at.unix_timestamp(),
            started_at.nanosecond()
        );
        let snapshot_result = self
            .environment_service()
            .create_snapshot_locked_from_preparation(
                CreateEnvSnapshotOptions {
                    env_name: env_name.to_string(),
                    label: Some(snapshot_label.to_string()),
                },
                Some((env_meta.service_enabled, env_meta.service_running)),
                prepared,
            );
        let snapshot = match snapshot_result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                for backup in runtime_backups {
                    backup.cleanup();
                }
                let snapshot_error = format!(
                    "failed to create {snapshot_label} snapshot for env \"{env_name}\": {error}"
                );
                return match self
                    .service_service()
                    .restore_after_snapshot_locked(env_name, service_state)
                {
                    Ok(()) => Err(snapshot_error),
                    Err(service_error) => Err(format!(
                        "{snapshot_error}; also failed to restore the managed service after snapshot capture: {service_error}"
                    )),
                };
            }
        };

        let service_quiesced = service_state.is_some();

        Ok(UpgradeTransaction {
            id,
            snapshot_id: snapshot.id,
            runtime_backups,
            created_runtime_names,
            rollback_enabled,
            started_at,
            source: plan.source,
            target: plan.target,
            service_before: UpgradeHistoryServiceState {
                enabled: env_meta.service_enabled,
                running: env_meta.service_running,
            },
            migration: UpgradeHistoryStage {
                status: "not-run".to_string(),
                note: None,
            },
            finalization: UpgradeHistoryStage {
                status: "not-run".to_string(),
                note: None,
            },
            service_quiesced,
            mutated_runtime_names: BTreeSet::new(),
            rollback_of,
        })
    }

    fn finish_successful_upgrade(
        &self,
        summary: UpgradeEnvSummary,
        mut transaction: UpgradeTransaction,
    ) -> Result<UpgradeEnvSummary, String> {
        if let Err(error) =
            self.retain_required_runtime_recovery(&summary.env_name, &mut transaction)
        {
            return self.rollback_failed_upgrade(
                &summary.env_name,
                &summary.previous_binding_kind,
                summary.previous_binding_name.clone(),
                &summary.binding_kind,
                summary.binding_name.clone(),
                summary.runtime_release_version.clone(),
                summary.runtime_release_channel.clone(),
                transaction,
                format!("failed to retain runtime recovery material: {error}"),
            );
        }
        if let Err(error) = self.record_upgrade_history(&transaction, &summary) {
            return self.rollback_failed_upgrade(
                &summary.env_name,
                &summary.previous_binding_kind,
                summary.previous_binding_name.clone(),
                &summary.binding_kind,
                summary.binding_name.clone(),
                summary.runtime_release_version.clone(),
                summary.runtime_release_channel.clone(),
                transaction,
                format!("failed to record upgrade history: {error}"),
            );
        }
        transaction.commit();
        Ok(summary)
    }

    fn retain_required_runtime_recovery(
        &self,
        env_name: &str,
        transaction: &mut UpgradeTransaction,
    ) -> Result<(), String> {
        if transaction.source.kind != "runtime"
            || transaction.source.name != transaction.target.name
        {
            return Ok(());
        }
        let runtime_name = transaction.source.name.clone();
        if !transaction.mutated_runtime_names.contains(&runtime_name) {
            return Ok(());
        }
        let Some(backup) = transaction
            .runtime_backups
            .iter_mut()
            .find(|backup| backup.meta.name == runtime_name)
        else {
            return Err(format!(
                "runtime backup for in-place upgrade of \"{runtime_name}\" was not created"
            ));
        };
        let Some(source_root) = backup.backup_root.take() else {
            return Err(format!(
                "runtime \"{runtime_name}\" does not have installer-managed bytes to retain"
            ));
        };
        let transaction_recovery_root =
            upgrade_history_recovery_dir(env_name, &transaction.id, &self.env, &self.cwd)?;
        let recovery_root = upgrade_history_runtime_recovery_dir(
            env_name,
            &transaction.id,
            &runtime_name,
            &self.env,
            &self.cwd,
        )?;
        if transaction_recovery_root.exists() {
            backup.backup_root = Some(source_root);
            return Err(format!(
                "runtime recovery path already exists: {}",
                display_path(&transaction_recovery_root)
            ));
        }
        fs::create_dir_all(&recovery_root).map_err(|error| error.to_string())?;
        if let Err(error) = fs::write(
            transaction_recovery_root.join("snapshot-id"),
            &transaction.snapshot_id,
        ) {
            let _ = fs::remove_dir_all(&transaction_recovery_root);
            backup.backup_root = Some(source_root);
            return Err(format!(
                "failed to record the recovery snapshot at {}: {error}",
                display_path(&transaction_recovery_root)
            ));
        }
        let recovery_install_root = recovery_root.join("install-root");
        if let Err(error) = fs::rename(&source_root, &recovery_install_root) {
            let _ = fs::remove_dir_all(&transaction_recovery_root);
            backup.backup_root = Some(source_root);
            return Err(format!(
                "failed to retain runtime recovery bytes at {}: {error}",
                display_path(&recovery_install_root)
            ));
        }
        backup.backup_root = Some(recovery_install_root);
        backup.retained_root = Some(transaction_recovery_root);
        write_json(&recovery_root.join("runtime.json"), &backup.meta)?;
        backup.backup_id = Some(runtime_name);
        Ok(())
    }

    fn record_upgrade_history(
        &self,
        transaction: &UpgradeTransaction,
        summary: &UpgradeEnvSummary,
    ) -> Result<(), String> {
        let env_meta = self.environment_service().get(&summary.env_name)?;
        let runtime_recovery = transaction
            .runtime_backups
            .iter()
            .filter(|backup| {
                transaction
                    .mutated_runtime_names
                    .contains(&backup.meta.name)
            })
            .map(|backup| UpgradeHistoryRuntimeRecovery {
                runtime_name: backup.meta.name.clone(),
                release_version: backup.meta.release_version.clone(),
                backup_id: backup.backup_id.clone(),
            })
            .collect();
        let record = UpgradeHistoryRecord {
            kind: "ocm-upgrade-transaction".to_string(),
            format_version: 1,
            id: transaction.id.clone(),
            env_name: summary.env_name.clone(),
            source: transaction.source.clone(),
            target: transaction.target.clone(),
            snapshot_id: transaction.snapshot_id.clone(),
            runtime_recovery,
            started_at: transaction.started_at,
            completed_at: time::OffsetDateTime::now_utc(),
            outcome: summary.outcome.clone(),
            migration: transaction.migration.clone(),
            finalization: transaction.finalization.clone(),
            service_before: transaction.service_before.clone(),
            service_after: UpgradeHistoryServiceState {
                enabled: env_meta.service_enabled,
                running: env_meta.service_running,
            },
            rollback: summary.rollback.clone(),
            rollback_of: transaction.rollback_of.clone(),
            note: None,
        };
        save_upgrade_history_record(&record, &self.env, &self.cwd)
    }

    fn backup_runtime_for_upgrade(
        &self,
        runtime: &RuntimeMeta,
    ) -> Result<RuntimeRollbackBackup, String> {
        let install_root = runtime_install_root(&runtime.name, &self.env, &self.cwd)?;
        let backup_root = if runtime
            .install_root
            .as_deref()
            .map(Path::new)
            .map(clean_path)
            .is_some_and(|path| path == install_root)
            && install_root.exists()
        {
            let parent = upgrade_backup_parent(&self.env, &self.cwd)?;
            fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
            let backup_root = parent.join(format!(
                "{}-{}-{}",
                runtime.name,
                std::process::id(),
                time::OffsetDateTime::now_utc().unix_timestamp_nanos()
            ));
            copy_dir_recursive(&install_root, &backup_root)?;
            Some(backup_root)
        } else {
            None
        };

        Ok(RuntimeRollbackBackup {
            meta: runtime.clone(),
            backup_root,
            retained_root: None,
            backup_id: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn fail_upgrade_before_cutover(
        &self,
        env_name: &str,
        previous_binding_kind: &str,
        previous_binding_name: String,
        binding_kind: &str,
        binding_name: String,
        runtime_release_version: Option<String>,
        runtime_release_channel: Option<String>,
        error: String,
    ) -> Result<UpgradeEnvSummary, String> {
        Ok(UpgradeEnvSummary {
            env_name: env_name.to_string(),
            previous_binding_kind: previous_binding_kind.to_string(),
            previous_binding_name,
            binding_kind: binding_kind.to_string(),
            binding_name,
            outcome: "failed".to_string(),
            runtime_release_version,
            runtime_release_channel,
            service_action: None,
            snapshot_id: None,
            rollback: None,
            note: Some(format!(
                "upgrade preparation failed before cutover; the source environment and service were left unchanged: {error}"
            )),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn rollback_failed_upgrade(
        &self,
        env_name: &str,
        previous_binding_kind: &str,
        previous_binding_name: String,
        binding_kind: &str,
        binding_name: String,
        runtime_release_version: Option<String>,
        runtime_release_channel: Option<String>,
        transaction: UpgradeTransaction,
        error: String,
    ) -> Result<UpgradeEnvSummary, String> {
        if !transaction.rollback_enabled {
            let snapshot_id = transaction.snapshot_id.clone();
            let service_restore_warning = if transaction.service_before.enabled
                && transaction.service_before.running
            {
                self.service_service()
                    .start_locked(env_name)
                    .err()
                    .map(|restore_error| {
                        format!("failed to restore the pre-upgrade service state: {restore_error}")
                    })
            } else {
                None
            };
            let mut summary = UpgradeEnvSummary {
                env_name: env_name.to_string(),
                previous_binding_kind: previous_binding_kind.to_string(),
                previous_binding_name,
                binding_kind: binding_kind.to_string(),
                binding_name,
                outcome: "failed".to_string(),
                runtime_release_version,
                runtime_release_channel,
                service_action: None,
                snapshot_id: Some(snapshot_id),
                rollback: Some("disabled".to_string()),
                note: join_optional_warnings(
                    Some(format!("upgrade failed and rollback was disabled: {error}")),
                    service_restore_warning,
                ),
            };
            if let Err(history_error) = self.record_upgrade_history(&transaction, &summary) {
                summary.note = join_optional_warnings(
                    summary.note,
                    Some(format!("upgrade history was not recorded: {history_error}")),
                );
            }
            transaction.cleanup();
            return Ok(summary);
        }

        let rollback_result = self.rollback_upgrade_locked(env_name, &transaction);
        let snapshot_id = transaction.snapshot_id.clone();
        let mut summary = match rollback_result {
            Ok(()) => UpgradeEnvSummary {
                env_name: env_name.to_string(),
                previous_binding_kind: previous_binding_kind.to_string(),
                previous_binding_name,
                binding_kind: binding_kind.to_string(),
                binding_name,
                outcome: "rolled-back".to_string(),
                runtime_release_version,
                runtime_release_channel,
                service_action: None,
                snapshot_id: Some(snapshot_id),
                rollback: Some("restored".to_string()),
                note: Some(format!(
                    "upgrade failed, so ocm restored the pre-upgrade snapshot: {error}"
                )),
            },
            Err(rollback_error) => UpgradeEnvSummary {
                env_name: env_name.to_string(),
                previous_binding_kind: previous_binding_kind.to_string(),
                previous_binding_name,
                binding_kind: binding_kind.to_string(),
                binding_name,
                outcome: "rollback-failed".to_string(),
                runtime_release_version,
                runtime_release_channel,
                service_action: None,
                snapshot_id: Some(snapshot_id),
                rollback: Some("failed".to_string()),
                note: Some(format!(
                    "upgrade failed ({error}); rollback also failed: {rollback_error}"
                )),
            },
        };
        if let Err(history_error) = self.record_upgrade_history(&transaction, &summary) {
            summary.note = join_optional_warnings(
                summary.note,
                Some(format!("upgrade history was not recorded: {history_error}")),
            );
        }
        transaction.cleanup();
        Ok(summary)
    }

    fn rollback_upgrade_locked(
        &self,
        env_name: &str,
        transaction: &UpgradeTransaction,
    ) -> Result<(), String> {
        let changes_runtime_trees = !transaction.mutated_runtime_names.is_empty();
        let changes_binding = transaction.source.kind != transaction.target.kind
            || transaction.source.name != transaction.target.name;
        if changes_runtime_trees || changes_binding {
            self.service_service()
                .quiesce_for_runtime_mutation_locked(env_name)
                .map_err(|error| {
                    format!("failed to quiesce the failed target before upgrade rollback: {error}")
                })?;
        }
        for runtime_backup in transaction.runtime_backups.iter().filter(|backup| {
            transaction
                .mutated_runtime_names
                .contains(&backup.meta.name)
        }) {
            self.restore_runtime_backup(runtime_backup)?;
        }
        self.environment_service()
            .restore_snapshot_locked(RestoreEnvSnapshotOptions {
                env_name: env_name.to_string(),
                snapshot_id: transaction.snapshot_id.clone(),
            })?;
        if changes_runtime_trees {
            self.service_service().wait_for_binding_convergence_locked(
                env_name,
                &transaction.source.kind,
                &transaction.source.name,
            )?;
        }
        for runtime_name in transaction
            .created_runtime_names
            .iter()
            .filter(|runtime_name| transaction.mutated_runtime_names.contains(*runtime_name))
        {
            self.remove_runtime_created_during_upgrade(runtime_name)?;
        }
        if transaction.service_before.enabled && transaction.service_before.running {
            self.service_service()
                .start_locked(env_name)
                .map(|_| ())
                .map_err(|error| format!("failed to restart the restored service: {error}"))?;
        }
        Ok(())
    }

    fn remove_runtime_created_during_upgrade(&self, runtime_name: &str) -> Result<(), String> {
        let meta_path = runtime_meta_path(runtime_name, &self.env, &self.cwd)?;
        if !meta_path.exists() {
            return Ok(());
        }
        remove_runtime(runtime_name, &self.env, &self.cwd).map(|_| ())
    }

    fn restore_runtime_backup(&self, backup: &RuntimeRollbackBackup) -> Result<(), String> {
        let meta_path = runtime_meta_path(&backup.meta.name, &self.env, &self.cwd)?;
        if let Some(backup_root) = backup.backup_root.as_ref() {
            let install_root = runtime_install_root(&backup.meta.name, &self.env, &self.cwd)?;
            if install_root.exists() {
                fs::remove_dir_all(&install_root).map_err(|error| {
                    format!(
                        "failed to remove upgraded runtime root {}: {error}",
                        display_path(&install_root)
                    )
                })?;
            }
            copy_dir_recursive(backup_root, &install_root)?;
        }
        write_json(&meta_path, &backup.meta)
    }
}

struct PreparedUpgradeTarget {
    name: String,
    prepared_binary_path: PathBuf,
    meta: RuntimeMeta,
    action: OfficialRuntimePrepareAction,
    staged: Option<StagedRuntimeInstall>,
}

impl PreparedUpgradeTarget {
    fn commit(mut self) -> Result<Self, String> {
        if let Some(staged) = self.staged.take() {
            self.meta = staged.commit()?;
        }
        Ok(self)
    }
}

#[derive(Debug)]
struct SimulationCommandOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl SimulationCommandOutput {
    fn from_output(output: std::process::Output) -> Self {
        Self {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn first_line(&self) -> String {
        summarize_command_text(&self.stdout, &self.stderr).unwrap_or_else(|| "ok".to_string())
    }

    fn failure_summary(&self) -> String {
        let detail = summarize_command_text(&self.stderr, &self.stdout)
            .unwrap_or_else(|| "no output".to_string());
        format!(
            "exited with code {}: {detail}",
            self.status.code().unwrap_or(1)
        )
    }
}

impl UpgradeSimulationCheck {
    fn passed(name: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "passed".to_string(),
            note: Some(note.into()),
        }
    }

    fn skipped(name: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "skipped".to_string(),
            note: Some(note.into()),
        }
    }

    fn failed(name: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "failed".to_string(),
            note: Some(note.into()),
        }
    }
}

impl UpgradeSimulationTarget {
    fn display(&self) -> String {
        match self {
            Self::Official { display, .. } | Self::LocalRepo { display, .. } => display.clone(),
        }
    }

    fn update_plan_args(&self) -> Option<Vec<String>> {
        match self {
            Self::Official { target, .. } => {
                let mut args = vec![
                    "update".to_string(),
                    "--dry-run".to_string(),
                    "--json".to_string(),
                    "--no-restart".to_string(),
                    "--yes".to_string(),
                ];
                if let Some(channel) = target.channel.as_deref() {
                    args.push("--channel".to_string());
                    args.push(channel.to_string());
                } else if let Some(version) = target.version.as_deref() {
                    args.push("--tag".to_string());
                    args.push(version.to_string());
                }
                Some(args)
            }
            Self::LocalRepo { .. } => None,
        }
    }
}

impl UpgradeSimulationScenario {
    fn parse_many(raw: Option<&str>) -> Result<Vec<Self>, String> {
        let Some(raw) = raw else {
            return Ok(vec![Self::Current]);
        };
        let mut scenarios: Vec<Self> = Vec::new();
        for token in raw.split(',') {
            let token = token.trim().to_ascii_lowercase();
            if token.is_empty() {
                return Err("--scenario cannot contain an empty scenario".to_string());
            }
            if token == "all" {
                return Ok(vec![Self::Current, Self::Minimum, Self::Telegram]);
            }
            let scenario = match token.as_str() {
                "current" | "source" => Self::Current,
                "minimum" | "clean" => Self::Minimum,
                "telegram" => Self::Telegram,
                _ => {
                    return Err(format!(
                        "unknown upgrade simulation scenario \"{token}\"; use current, minimum, telegram, or all"
                    ));
                }
            };
            if !scenarios
                .iter()
                .any(|existing| existing.id() == scenario.id())
            {
                scenarios.push(scenario);
            }
        }
        if scenarios.is_empty() {
            return Err("--scenario requires current, minimum, telegram, or all".to_string());
        }
        Ok(scenarios)
    }

    fn id(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Minimum => "minimum",
            Self::Telegram => "telegram",
        }
    }
}

#[derive(Debug)]
struct UpgradeTransaction {
    id: String,
    snapshot_id: String,
    runtime_backups: Vec<RuntimeRollbackBackup>,
    created_runtime_names: Vec<String>,
    rollback_enabled: bool,
    started_at: time::OffsetDateTime,
    source: UpgradeHistoryBinding,
    target: UpgradeHistoryBinding,
    service_before: UpgradeHistoryServiceState,
    migration: UpgradeHistoryStage,
    finalization: UpgradeHistoryStage,
    service_quiesced: bool,
    mutated_runtime_names: BTreeSet<String>,
    rollback_of: Option<String>,
}

impl UpgradeTransaction {
    fn mark_runtime_mutated(&mut self, runtime_name: &str) {
        self.mutated_runtime_names.insert(runtime_name.to_string());
    }

    fn mark_post_update_completed(&mut self, note: Option<&str>) {
        self.migration.status = if note.is_some_and(|note| note.contains("config repair")) {
            "repaired".to_string()
        } else {
            "validated".to_string()
        };
        self.finalization.status = "completed".to_string();
    }

    fn mark_post_update_failed(&mut self, error: &str) {
        if error.contains("openclaw update finalize failed") {
            self.migration.status = "validated".to_string();
            self.finalization.status = "failed".to_string();
        } else {
            self.migration.status = "failed".to_string();
            self.finalization.status = "not-run".to_string();
        }
    }

    fn mark_post_update_not_needed(&mut self) {
        self.migration.status = "not-needed".to_string();
        self.finalization.status = "not-needed".to_string();
    }

    fn cleanup(self) {
        for runtime_backup in self.runtime_backups {
            runtime_backup.cleanup();
        }
    }

    fn commit(self) {
        for runtime_backup in self.runtime_backups {
            runtime_backup.commit();
        }
    }
}

#[derive(Debug)]
struct RuntimeRollbackBackup {
    meta: RuntimeMeta,
    backup_root: Option<PathBuf>,
    retained_root: Option<PathBuf>,
    backup_id: Option<String>,
}

impl RuntimeRollbackBackup {
    fn cleanup(mut self) {
        if let Some(retained_root) = self.retained_root.take() {
            self.backup_root.take();
            let _ = fs::remove_dir_all(retained_root);
        } else if let Some(backup_root) = self.backup_root.take() {
            let _ = fs::remove_dir_all(backup_root);
        }
    }

    fn commit(mut self) {
        if self.backup_id.is_some() {
            self.backup_root.take();
            self.retained_root.take();
        } else {
            self.cleanup();
        }
    }
}

impl Drop for RuntimeRollbackBackup {
    fn drop(&mut self) {
        if let Some(retained_root) = self.retained_root.take() {
            self.backup_root.take();
            let _ = fs::remove_dir_all(retained_root);
        } else if let Some(backup_root) = self.backup_root.take() {
            let _ = fs::remove_dir_all(backup_root);
        }
    }
}

fn upgrade_backup_parent(
    env: &std::collections::BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    Ok(ensure_store(env, cwd)?
        .home
        .join("tmp")
        .join("upgrade-runtime-backups"))
}

fn source_binding(env: &crate::env::EnvMeta) -> (String, String) {
    if let Some(runtime) = env.default_runtime.clone() {
        return ("runtime".to_string(), runtime);
    }
    if let Some(launcher) = env.default_launcher.clone() {
        return ("launcher".to_string(), launcher);
    }
    if env.dev.is_some() {
        return ("dev".to_string(), "dev".to_string());
    }
    ("none".to_string(), "none".to_string())
}

fn is_rollback_candidate(record: &UpgradeHistoryRecord) -> bool {
    matches!(record.outcome.as_str(), "updated" | "switched")
        || (record.outcome == "rolled-back"
            && record.rollback_of.is_some()
            && record.rollback.is_none())
}

fn has_successful_rollback_child(history: &[UpgradeHistoryRecord], transaction_id: &str) -> bool {
    history.iter().any(|record| {
        record.rollback_of.as_deref() == Some(transaction_id) && record.outcome == "rolled-back"
    })
}

fn rollback_service_action_for_dry_run(record: &UpgradeHistoryRecord) -> Option<String> {
    if record.service_before.enabled && record.service_before.running {
        Some("would-start".to_string())
    } else if record.service_after.enabled && record.service_after.running {
        Some("would-stop".to_string())
    } else {
        None
    }
}

fn build_simulation_batch_summary(
    summaries: Vec<UpgradeSimulationSummary>,
) -> UpgradeSimulationBatchSummary {
    let source_env = summaries
        .first()
        .map(|summary| summary.source_env.clone())
        .unwrap_or_default();
    let to = summaries
        .first()
        .map(|summary| summary.to.clone())
        .unwrap_or_default();
    let failed = summaries
        .iter()
        .filter(|summary| summary.outcome == "failed")
        .count();
    UpgradeSimulationBatchSummary {
        source_env,
        to,
        count: summaries.len(),
        passed: summaries.len().saturating_sub(failed),
        failed,
        results: summaries,
    }
}

fn missing_simulation_version_error(version: &str, releases: &[OpenClawRelease]) -> String {
    let prefix = format!("{version}-");
    let nearby = releases
        .iter()
        .filter(|release| release.version.starts_with(&prefix))
        .map(|release| release.version.as_str())
        .take(5)
        .collect::<Vec<_>>();

    let mut message = format!(
        "OpenClaw release version \"{version}\" was not found; simulation did not create any scenario envs"
    );
    if !nearby.is_empty() {
        message.push_str(&format!(
            ". Nearby published releases: {}",
            nearby.join(", ")
        ));
    }
    message.push_str(
        ". Use an exact published version, a channel such as beta, or a local OpenClaw repo path.",
    );
    message
}

fn version_output_matches_expected(actual: &str, expected: &str) -> bool {
    let actual = actual.trim();
    if actual == expected {
        return true;
    }

    actual
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '+'))
        .any(|token| token == expected)
}

fn release_version_from_output(actual: &str, hint: Option<&str>) -> Option<String> {
    if let Some(hint) = hint
        && version_output_matches_expected(actual, hint)
        && compare_runtime_release_versions(hint, hint).is_some()
    {
        return Some(hint.to_string());
    }

    actual
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '+'))
        .find(|token| !token.is_empty() && compare_runtime_release_versions(token, token).is_some())
        .map(str::to_string)
}

fn simulation_env_name(source_name: &str, scenario: &str) -> String {
    format!(
        "{}-{}-sim-{}",
        source_name,
        scenario,
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    )
}

fn simulation_runtime_name(source_name: &str) -> String {
    format!(
        "ocm-sim-runtime-{}-{}",
        source_name,
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    )
}

fn reset_to_minimum_simulation_config(
    paths: &crate::store::EnvPaths,
    gateway_port: u32,
) -> Result<(), String> {
    if let Some(parent) = paths.config_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if paths.config_path.exists() {
        fs::remove_file(&paths.config_path).map_err(|error| error.to_string())?;
    }
    ensure_minimum_local_openclaw_config(paths, gateway_port)
}

fn seed_telegram_simulation_config(paths: &crate::store::EnvPaths) -> Result<(), String> {
    let raw = fs::read_to_string(&paths.config_path).map_err(|error| error.to_string())?;
    let mut value: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| "OpenClaw config root must be an object".to_string())?;

    let channels = ensure_json_object_field(root, "channels");
    channels.insert(
        "telegram".to_string(),
        json!({
            "enabled": true,
            "botToken": "123456:simulation-token",
            "allowFrom": ["*"],
            "groupPolicy": "open"
        }),
    );

    let plugins = ensure_json_object_field(root, "plugins");
    let mut allow = plugins
        .get("allow")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !allow.iter().any(|entry| entry.as_str() == Some("telegram")) {
        allow.push(Value::String("telegram".to_string()));
    }
    plugins.insert("allow".to_string(), Value::Array(allow));

    let mut rewritten = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    rewritten.push('\n');
    fs::write(&paths.config_path, rewritten).map_err(|error| error.to_string())
}

fn ensure_json_object_field<'a>(
    object: &'a mut serde_json::Map<String, Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, Value> {
    let needs_reset = !object.get(key).is_some_and(Value::is_object);
    if needs_reset {
        object.insert(key.to_string(), Value::Object(serde_json::Map::new()));
    }
    object
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("object field must exist after reset")
}

fn summarize_command_text(primary: &str, secondary: &str) -> Option<String> {
    for text in [primary, secondary] {
        if let Some(line) = text.lines().find_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then_some(trimmed.to_string())
        }) {
            return Some(line);
        }
    }
    None
}

fn summarize_command_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    summarize_command_text(&stderr, &stdout).unwrap_or_else(|| "no output".to_string())
}

fn command_output_reports_unsupported_command(stdout: &str, stderr: &str) -> bool {
    [stdout, stderr].iter().any(|text| {
        let normalized = text.to_ascii_lowercase();
        [
            "unknown command",
            "unrecognized command",
            "command not found",
            "unexpected args:",
        ]
        .iter()
        .any(|marker| normalized.contains(marker))
    })
}

fn candidate_codex_preflight_is_unsupported(stdout: &str, stderr: &str) -> bool {
    const CHECK_ID: &str = "codex/managed-app-server";
    if let Ok(value) = serde_json::from_str::<Value>(stdout)
        && value
            .get("findings")
            .and_then(Value::as_array)
            .is_some_and(|findings| {
                findings.iter().any(|finding| {
                    finding.get("checkId").and_then(Value::as_str)
                        == Some("core/doctor/lint-selection")
                        && finding.get("path").and_then(Value::as_str) == Some(CHECK_ID)
                        && finding
                            .get("message")
                            .and_then(Value::as_str)
                            .is_some_and(|message| {
                                message.contains("Unknown health check id selected by --only")
                            })
                })
            })
    {
        return true;
    }

    let normalized = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    [
        "unknown command 'doctor'",
        "unknown command \"doctor\"",
        "unrecognized command 'doctor'",
        "unrecognized command \"doctor\"",
        "unknown option '--lint'",
        "unknown option \"--lint\"",
        "unrecognized option '--lint'",
        "unrecognized option \"--lint\"",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn shell_command(command: &str) -> Command {
    if cfg!(windows) {
        let mut process = Command::new("cmd");
        process.args(["/C", command]);
        process
    } else {
        let mut process = Command::new("/bin/sh");
        process.args(["-lc", command]);
        process
    }
}

fn service_action_for_dry_run(
    service: Option<&ServiceSummary>,
    binding_changed: bool,
    runtime_changed: bool,
) -> Option<String> {
    let service = service?;
    if !service.installed || !service.desired_running || (!binding_changed && !runtime_changed) {
        return None;
    }
    if service.running {
        Some("would-restart".to_string())
    } else {
        Some("would-start".to_string())
    }
}

fn is_changed_upgrade_outcome(outcome: &str) -> bool {
    matches!(
        outcome,
        "updated" | "switched" | "would-update" | "would-switch"
    )
}

fn is_failed_upgrade_outcome(outcome: &str) -> bool {
    matches!(outcome, "failed" | "rolled-back" | "rollback-failed")
}

fn outcome_for_official_prepare_action(action: &OfficialRuntimePrepareAction) -> String {
    match action {
        OfficialRuntimePrepareAction::Installed | OfficialRuntimePrepareAction::Updated => {
            "updated".to_string()
        }
        OfficialRuntimePrepareAction::Reused => "up-to-date".to_string(),
    }
}

fn note_for_official_prepare_action(action: &OfficialRuntimePrepareAction) -> Option<String> {
    match action {
        OfficialRuntimePrepareAction::Installed => {
            Some("installed the requested runtime".to_string())
        }
        OfficialRuntimePrepareAction::Updated => Some("updated the tracked runtime".to_string()),
        OfficialRuntimePrepareAction::Reused => None,
    }
}

fn join_warnings(warnings: &[String]) -> Option<String> {
    if warnings.is_empty() {
        None
    } else {
        Some(warnings.join(" "))
    }
}

fn join_optional_warnings(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => Some(format!("{left} {right}")),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn verify_gateway_status_readiness(stdout: &str) -> Result<(), String> {
    let status: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        format!("post-upgrade gateway readiness failed: invalid status JSON ({error})")
    })?;

    if let Some(ready) = status.pointer("/rpc/ok").and_then(Value::as_bool) {
        return gateway_readiness_result(ready, &status);
    }
    if let Some(ready) = status.get("ok").and_then(Value::as_bool) {
        return gateway_readiness_result(ready, &status);
    }
    if let Some(targets) = status.get("targets").and_then(Value::as_array) {
        let ready = targets.iter().any(|target| {
            target
                .pointer("/connect/ok")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        return gateway_readiness_result(ready, &status);
    }

    Err(
        "post-upgrade gateway readiness failed: status JSON did not report RPC reachability"
            .to_string(),
    )
}

fn gateway_readiness_result(ready: bool, status: &Value) -> Result<(), String> {
    if ready {
        return Ok(());
    }

    let rpc_error = status.pointer("/rpc/error").and_then(Value::as_str);
    if rpc_error.is_some_and(gateway_auth_failure_proves_reachable) {
        return Ok(());
    }
    let target_auth_reachable = status
        .get("targets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|target| target.pointer("/connect/error").and_then(Value::as_str))
        .any(gateway_auth_failure_proves_reachable);
    if target_auth_reachable {
        return Ok(());
    }
    let target_error = status
        .get("targets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|target| target.pointer("/connect/error").and_then(Value::as_str));

    let detail = rpc_error
        .or_else(|| {
            status
                .get("warnings")
                .and_then(Value::as_array)
                .and_then(|warnings| warnings.first())
                .and_then(|warning| warning.get("message"))
                .and_then(Value::as_str)
        })
        .or(target_error)
        .unwrap_or("OpenClaw reported that no gateway RPC endpoint was reachable");

    Err(format!("post-upgrade gateway readiness failed: {detail}"))
}

fn gateway_auth_failure_proves_reachable(error: &str) -> bool {
    let normalized = error.trim().to_ascii_lowercase();
    let Some(reason) = normalized
        .strip_prefix("gateway closed (1008):")
        .map(str::trim)
    else {
        return normalized == "device identity required";
    };

    matches!(
        reason,
        "auth required"
            | "owner auth required"
            | "connect failed"
            | "device identity required"
            | "device required"
            | "pairing required"
    ) || reason.starts_with("pairing required:")
        || reason.starts_with("unauthorized: gateway token missing")
        || reason.starts_with("unauthorized: gateway token mismatch")
        || reason.starts_with("unauthorized: gateway token not configured")
        || reason.starts_with("unauthorized: gateway password missing")
        || reason.starts_with("unauthorized: gateway password mismatch")
        || reason.starts_with("unauthorized: gateway password not configured")
        || reason.starts_with("unauthorized: bootstrap token invalid or expired")
        || reason.starts_with("unauthorized: tailscale identity missing")
        || reason.starts_with("unauthorized: tailscale proxy headers missing")
        || reason.starts_with("unauthorized: tailscale identity check failed")
        || reason.starts_with("unauthorized: tailscale identity mismatch")
        || reason.starts_with("unauthorized: too many failed authentication attempts")
        || reason.starts_with("unauthorized: device token mismatch")
        || reason.starts_with("unauthorized: device token rejected")
}

#[cfg(test)]
mod tests {
    use super::{
        candidate_codex_preflight_is_unsupported, command_output_reports_unsupported_command,
        release_version_from_output, verify_gateway_status_readiness,
        version_output_matches_expected,
    };

    #[test]
    fn version_output_accepts_exact_version() {
        assert!(version_output_matches_expected("2026.5.14", "2026.5.14"));
    }

    #[test]
    fn version_output_accepts_decorated_openclaw_version() {
        assert!(version_output_matches_expected(
            "OpenClaw 2026.5.14 (62375ae)",
            "2026.5.14"
        ));
    }

    #[test]
    fn version_output_accepts_beta_version_tokens() {
        assert!(version_output_matches_expected(
            "OpenClaw 2026.5.12-beta.8 (local)",
            "2026.5.12-beta.8"
        ));
    }

    #[test]
    fn version_output_rejects_partial_token_matches() {
        assert!(!version_output_matches_expected(
            "OpenClaw 12026.5.14",
            "2026.5.14"
        ));
    }

    #[test]
    fn release_version_output_extracts_openclaw_and_generic_versions() {
        assert_eq!(
            release_version_from_output("OpenClaw 2026.7.1-2 (local)", None).as_deref(),
            Some("2026.7.1-2")
        );
        assert_eq!(
            release_version_from_output("runtime 0.3.0", None).as_deref(),
            Some("0.3.0")
        );
        assert_eq!(
            release_version_from_output("OpenClaw current-main", Some("2026.7.2")).as_deref(),
            None
        );
    }

    #[test]
    fn gateway_readiness_accepts_historical_and_current_payloads() {
        assert!(
            verify_gateway_status_readiness(
                r#"{"rpc":{"ok":true,"server":{"version":"2026.6.11"}}}"#
            )
            .is_ok()
        );
        assert!(
            verify_gateway_status_readiness(
                r#"{"ok":true,"targets":[{"connect":{"ok":true,"rpcOk":true}}]}"#
            )
            .is_ok()
        );
        assert!(
            verify_gateway_status_readiness(
                r#"{"targets":[{"connect":{"ok":false}},{"connect":{"ok":true}}]}"#
            )
            .is_ok()
        );
        assert!(
            verify_gateway_status_readiness(
                r#"{"rpc":{"ok":false,"error":"device identity required"}}"#
            )
            .is_ok()
        );
        assert!(
            verify_gateway_status_readiness(
                r#"{"rpc":{"ok":false,"error":"gateway closed (1008): unauthorized: gateway token mismatch"}}"#
            )
            .is_ok()
        );
        assert!(
            verify_gateway_status_readiness(
                r#"{"ok":false,"targets":[{"connect":{"ok":false,"error":"gateway closed (1008): device identity required"}}]}"#
            )
            .is_ok()
        );
        assert!(
            verify_gateway_status_readiness(
                r#"{"targets":[{"connect":{"ok":false,"error":"connect ECONNREFUSED 127.0.0.1:18789"}},{"connect":{"ok":false,"error":"gateway closed (1008): pairing required"}}]}"#
            )
            .is_ok()
        );
    }

    #[test]
    fn gateway_readiness_rejects_unreachable_payloads_with_diagnostics() {
        let historical = verify_gateway_status_readiness(
            r#"{"rpc":{"ok":false,"error":"connect ECONNREFUSED 127.0.0.1:18789"}}"#,
        )
        .unwrap_err();
        assert!(
            historical.contains("connect ECONNREFUSED 127.0.0.1:18789"),
            "{historical}"
        );

        let current = verify_gateway_status_readiness(
            r#"{"ok":false,"warnings":[{"message":"No gateway answered any probe"}],"targets":[]}"#,
        )
        .unwrap_err();
        assert!(
            current.contains("No gateway answered any probe"),
            "{current}"
        );

        let arbitrary_auth_error = verify_gateway_status_readiness(
            r#"{"rpc":{"ok":false,"error":"authentication backend unavailable"}}"#,
        )
        .unwrap_err();
        assert!(
            arbitrary_auth_error.contains("authentication backend unavailable"),
            "{arbitrary_auth_error}"
        );

        let bare_connect_failure =
            verify_gateway_status_readiness(r#"{"rpc":{"ok":false,"error":"connect failed"}}"#)
                .unwrap_err();
        assert!(
            bare_connect_failure.contains("connect failed"),
            "{bare_connect_failure}"
        );
    }

    #[test]
    fn gateway_readiness_rejects_invalid_or_unknown_json() {
        let invalid = verify_gateway_status_readiness("not-json").unwrap_err();
        assert!(invalid.contains("invalid status JSON"), "{invalid}");

        let unknown = verify_gateway_status_readiness(r#"{"gatewayState":"running"}"#).unwrap_err();
        assert!(
            unknown.contains("did not report RPC reachability"),
            "{unknown}"
        );
    }

    #[test]
    fn config_probe_recognizes_unsupported_commands() {
        assert!(command_output_reports_unsupported_command(
            "",
            "error: unknown command 'config'"
        ));
        assert!(command_output_reports_unsupported_command(
            "",
            "unexpected args: config validate"
        ));
    }

    #[test]
    fn config_probe_keeps_invalid_config_failures_actionable() {
        assert!(!command_output_reports_unsupported_command(
            "",
            "OpenClaw config is invalid\nProblem: meta: Unrecognized key: lastTouchedAt"
        ));
    }

    #[test]
    fn codex_candidate_probe_only_skips_explicit_unsupported_results() {
        assert!(candidate_codex_preflight_is_unsupported(
            r#"{"findings":[{"checkId":"core/doctor/lint-selection","path":"codex/managed-app-server","message":"Unknown health check id selected by --only: codex/managed-app-server."}]}"#,
            ""
        ));
        assert!(candidate_codex_preflight_is_unsupported(
            "",
            "error: unknown option '--lint'"
        ));
        assert!(!candidate_codex_preflight_is_unsupported(
            r#"{"findings":[{"checkId":"codex/managed-app-server","path":"/candidate/codex","message":"version mismatch"}]}"#,
            ""
        ));
        assert!(!candidate_codex_preflight_is_unsupported(
            "",
            "managed Codex binary command not found"
        ));
    }
}
