use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use super::source::SourceInspection;
use super::{
    Cli, SimulationCommandOutput, UpgradeEnvSummary, UpgradeOptions, UpgradeTimingRecorder,
    UpgradeTransaction, UpgradeTransactionPlan, verify_gateway_build_id,
    verify_gateway_status_readiness,
};
use crate::infra::shell::build_openclaw_env;
use crate::launcher::parse_literal_launcher_command;
use crate::store::{
    EnvironmentOperationLock, ExclusiveFileLock, UpgradeHistoryBinding, lock_env_registry,
};

#[derive(Clone)]
struct ArtifactIdentity {
    commit: String,
    version: String,
    build_id: String,
}

fn text(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer)?.as_str().map(str::to_string)
}

fn artifact_identity(status: &Value) -> Option<ArtifactIdentity> {
    let artifact = status.pointer("/update/git/artifacts")?;
    if artifact.get("ready")?.as_bool()? != true {
        return None;
    }
    let commit = status.pointer("/update/git/sha")?.as_str()?;
    let version = artifact.get("version")?.as_str()?.trim();
    let build_id = super::openclaw_build_id(artifact)?;
    ((commit.len() == 40 || commit.len() == 64)
        && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !version.is_empty()
        && version.len() <= 128
        && !version.contains(char::is_control)
        && !build_id.contains(char::is_control))
    .then(|| ArtifactIdentity {
        commit: commit.to_string(),
        version: version.to_string(),
        build_id,
    })
}

fn source_failure_summary(output: &SimulationCommandOutput) -> String {
    // Native result extensions can contain plugin-owned data. Report only the
    // owned reason/error fields or bounded human diagnostics, never its JSON.
    let reason = serde_json::from_str::<Value>(output.stdout.trim())
        .ok()
        .and_then(|value| text(&value, "/reason"))
        .filter(|reason| !reason.trim().is_empty())
        .and_then(|reason| crate::infra::command_output::bounded_summary(reason.lines()));
    let detail = reason
        .or_else(|| super::diagnostics::readable_command_detail(&output.stdout))
        .or_else(|| super::diagnostics::readable_command_detail(&output.stderr))
        .unwrap_or_else(|| {
            "no readable command error; inspect native update status --json".to_string()
        });
    format!(
        "exited with code {}: {detail}",
        output.status.code().unwrap_or(1)
    )
}

fn summary(name: &str, launcher: &str, source: SourceInspection) -> UpgradeEnvSummary {
    UpgradeEnvSummary {
        source: Some(source),
        env_name: name.to_string(),
        previous_binding_kind: "launcher".to_string(),
        previous_binding_name: launcher.to_string(),
        binding_kind: "launcher".to_string(),
        binding_name: launcher.to_string(),
        outcome: "local-command".to_string(),
        runtime_release_version: None,
        runtime_release_channel: None,
        service_action: None,
        snapshot_id: None,
        rollback: None,
        note: None,
    }
}

impl Cli {
    fn admit_source_update(&self, name: &str, source: &SourceInspection) -> Result<(), String> {
        if source.working_tree_clean != Some(true) || !source.shared_environments.is_empty() {
            return Err(format!(
                "cannot update source for env {name:?}: {}",
                source.issues.join("; ")
            ));
        }
        self.environment_service()
            .ensure_source_watch_allows_state_mutation_locked(name)?;
        let root = Path::new(&source.root);
        // Git status intentionally trusts these index flags, so it cannot
        // establish whether tracked bytes are safe for native replacement.
        let index = super::source::inspection_git_command(root)
            .args(["ls-files", "--cached", "-v", "-z"])
            .output()
            .map_err(|error| error.to_string())?;
        if !index.status.success()
            || index
                .stdout
                .split(|byte| *byte == 0)
                .filter_map(|entry| entry.first())
                .any(|tag| tag.is_ascii_lowercase() || *tag == b'S')
        {
            return Err("cannot establish source cleanliness with unreadable, assume-unchanged, or skip-worktree index entries; inspect the checkout before upgrading".to_string());
        }
        let footprint = crate::store::dev_sources::inspect_source_footprint(root)?;
        for env in self.environment_service().list()? {
            // A temporary takeover keeps its old binding, so its live source
            // cannot be inferred from launcher/runtime registration alone.
            if env.name != name {
                let service = self.environment_service();
                match service.observe_source_watch(&env.name)? {
                    crate::env::SourceWatchState::Active(watch) => {
                        let watched = fs::canonicalize(&watch.repo_root).map_err(|error| {
                            format!("cannot inspect source owned by env {:?}: {error}", env.name)
                        })?;
                        if watched.starts_with(root) || root.starts_with(&watched) {
                            return Err(format!(
                                "cannot update source for env {name:?}: env {:?} has a foreground source session on this checkout",
                                env.name
                            ));
                        }
                    }
                    crate::env::SourceWatchState::Starting
                    | crate::env::SourceWatchState::Restoring => {
                        return Err(format!(
                            "cannot establish source isolation while env {:?} is starting or restoring its dev session",
                            env.name
                        ));
                    }
                    crate::env::SourceWatchState::Inactive => {
                        if service
                            .source_watch_session(&env.name)?
                            .is_some_and(|session| !session.closed)
                        {
                            return Err(format!(
                                "cannot establish source isolation while env {:?} has unfinished dev ownership",
                                env.name
                            ));
                        }
                    }
                }
                // Missing binding metadata is an admission failure, rather
                // than an empty observation of shared users.
                if let Some(launcher) = env.default_launcher.as_deref() {
                    let launcher = self.launcher_service().show(launcher)?;
                    if let Some(cwd) = launcher.cwd {
                        fs::canonicalize(cwd).map_err(|error| {
                            format!(
                                "cannot resolve launcher directory for env {:?}: {error}",
                                env.name
                            )
                        })?;
                    }
                }
                if let Some(runtime) = env.default_runtime.as_deref() {
                    let runtime = self.runtime_service().show(runtime)?;
                    fs::canonicalize(&runtime.binary_path).map_err(|error| {
                        format!(
                            "cannot resolve runtime binding for env {:?}: {error}",
                            env.name
                        )
                    })?;
                    if let Some(install_root) = runtime.install_root {
                        let installed = fs::canonicalize(install_root).map_err(|error| {
                            format!(
                                "cannot resolve runtime installation for env {:?}: {error}",
                                env.name
                            )
                        })?;
                        if root.starts_with(&installed) || installed.starts_with(root) {
                            return Err(format!(
                                "cannot update source for env {name:?}: checkout overlaps runtime files owned by env {:?}",
                                env.name
                            ));
                        }
                    }
                }
            }
            let env_root = fs::canonicalize(&env.root).map_err(|error| {
                format!(
                    "cannot inspect environment {:?} before source update: {error}",
                    env.name
                )
            })?;
            if root.starts_with(&env_root)
                || env_root.starts_with(root)
                || footprint
                    .entries
                    .iter()
                    .chain(&footprint.content_roots)
                    .any(|path| path.starts_with(&env_root))
            {
                return Err(format!(
                    "cannot update source for env {name:?}: checkout or Git metadata overlaps environment {:?}",
                    env.name
                ));
            }
        }
        Ok(())
    }

    fn source_command(
        &self,
        name: &str,
        launcher: &str,
        source: &SourceInspection,
        args: &[&str],
        operation: &EnvironmentOperationLock,
        registry: Option<&ExclusiveFileLock>,
    ) -> Result<SimulationCommandOutput, String> {
        let recipe = self.launcher_service().show(launcher)?;
        let tokens = parse_literal_launcher_command(&recipe.command)
            .ok_or("source launcher is no longer a literal command")?;
        let program = tokens.first().ok_or("source launcher is empty")?;
        let program = if matches!(
            Path::new(program).file_name().and_then(|p| p.to_str()),
            Some("node" | "node.exe")
        ) {
            let path = Path::new(program);
            if path.is_relative() && path.components().count() > 1 {
                crate::launcher::resolve_launcher_run_dir(&recipe, &self.cwd).join(path)
            } else {
                path.to_path_buf()
            }
        } else {
            "node".into()
        };
        let meta = self.environment_service().get(name)?;
        let mut env = build_openclaw_env(&meta, &self.env);
        // Execute the built public entrypoint without the source wrapper rebuilding first.
        env.remove("NODE_COMPILE_CACHE");
        env.insert("NODE_DISABLE_COMPILE_CACHE".to_string(), "1".to_string());
        let mut command = Command::new(program);
        command
            .arg(Path::new(&source.root).join("openclaw.mjs"))
            .args(args)
            .current_dir(&source.root)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The explicit checkout, rather than an invoking shell's Git context,
        // owns every Git subprocess started by the native updater.
        for (key, value) in crate::openclaw_repo::git_command().get_envs() {
            if value.is_none() {
                command.env_remove(key);
            }
        }
        command.env_remove("GIT_CONFIG");
        if let Some(registry) = registry {
            registry.retain_for_child(&mut command);
        }
        operation
            .output(command)
            .map(SimulationCommandOutput::from_output)
            .map_err(|error| format!("source OpenClaw command failed: {error}"))
    }

    fn source_status(
        &self,
        name: &str,
        launcher: &str,
        source: &SourceInspection,
        operation: &EnvironmentOperationLock,
        registry: Option<&ExclusiveFileLock>,
    ) -> Result<Value, String> {
        let output = self.source_command(
            name,
            launcher,
            source,
            &["update", "status", "--json"],
            operation,
            registry,
        )?;
        if !output.status.success() {
            return Err(format!(
                "native source status failed: {}",
                source_failure_summary(&output)
            ));
        }
        let status: Value = serde_json::from_str(output.stdout.trim())
            .map_err(|_| "native source status did not return valid JSON")?;
        let root = text(&status, "/update/root").ok_or("native source status has no root")?;
        if fs::canonicalize(root).ok().as_deref() != Some(Path::new(&source.root))
            || text(&status, "/update/installKind").as_deref() != Some("git")
        {
            return Err("native source status resolved a different installation".to_string());
        }
        Ok(status)
    }

    fn source_artifact_status(
        &self,
        name: &str,
        launcher: &str,
        source: &SourceInspection,
        operation: &EnvironmentOperationLock,
        transaction: &UpgradeTransaction,
    ) -> Result<Value, String> {
        // Recovery may already be handling an interrupt. A new interrupt during
        // this observation must not turn into another discovery command.
        let interrupted = transaction.interrupted();
        let output = self.source_command(
            name,
            launcher,
            source,
            &["status", "--json"],
            operation,
            None,
        )?;
        if transaction.interrupted() != interrupted
            || matches!(output.status.code(), None | Some(130 | 143))
        {
            return Err("native source observation was interrupted".to_string());
        }
        if let Ok(status) = serde_json::from_str::<Value>(output.stdout.trim()) {
            // Ordinary status also scans optional services and sessions. Missing
            // observations can use the canonical command, but facts that reject
            // this installation or its readiness must not be overwritten.
            if text(&status, "/update/root").is_some_and(|root| {
                fs::canonicalize(root).ok().as_deref() != Some(Path::new(&source.root))
            }) || text(&status, "/update/installKind")
                .is_some_and(|kind| kind != "git" && kind != "unknown")
            {
                return Err("native source status resolved a different installation".to_string());
            }
            if status
                .pointer("/update/git/artifacts/ready")
                .and_then(Value::as_bool)
                == Some(false)
            {
                return Err("native source artifacts are not ready".to_string());
            }
            if status
                .pointer("/error/code")
                .is_some_and(|code| !code.is_null())
                || text(&status, "/update/error/status").as_deref() == Some("failed")
            {
                return Err(format!(
                    "native source observation failed: {}",
                    source_failure_summary(&output)
                ));
            }
            if text(&status, "/update/git/sha")
                .zip(text(&status, "/update/git/builtSha"))
                .is_some_and(|(head, built)| head != built)
            {
                return Err("native source result does not match the resulting build".to_string());
            }
            if text(&status, "/update/root").is_some()
                && text(&status, "/update/installKind").as_deref() == Some("git")
                && text(&status, "/update/git/builtSha").is_some()
                && artifact_identity(&status).is_some()
                && status.pointer("/update/error").is_none_or(Value::is_null)
            {
                if !output.status.success() {
                    return Err(format!(
                        "native source observation failed: {}",
                        source_failure_summary(&output)
                    ));
                }
                return Ok(status);
            }
        }
        // Older or unconfigured status commands can omit local update facts.
        // This fallback retains the existing remote-discovery cost and guards.
        self.source_status(name, launcher, source, operation, None)
    }

    fn verify_source_gateway(
        &self,
        name: &str,
        launcher: &str,
        source: &SourceInspection,
        expected: &ArtifactIdentity,
        operation: &EnvironmentOperationLock,
    ) -> Result<(bool, String), String> {
        let output = self.source_command(
            name,
            launcher,
            source,
            &["gateway", "status", "--deep", "--json"],
            operation,
            None,
        )?;
        let status = verify_gateway_status_readiness(&output.stdout)?;
        let note = verify_gateway_build_id(&status, &expected.build_id)?;
        Ok((
            text(&status, "/rpc/server/buildId").as_deref() == Some(&expected.build_id),
            note.to_string(),
        ))
    }

    pub(super) fn upgrade_source_checkout(
        &self,
        name: &str,
        launcher: &str,
        source: SourceInspection,
        options: UpgradeOptions,
        operation: &EnvironmentOperationLock,
    ) -> Result<UpgradeEnvSummary, String> {
        let mut result = summary(name, launcher, source.clone());
        if !cfg!(any(target_os = "linux", target_os = "macos")) {
            result.note = Some("source updates require native child lock custody on this platform; update the checkout through its operator".to_string());
            return Ok(result);
        }
        if !options.rollback_enabled {
            return Err("source updates use native recovery; --no-rollback cannot disable the native updater's recovery policy".to_string());
        }
        if options.dry_run {
            result.note = Some("dry run: source inspection only; native update support and the remote target were not checked; no command, checkout, state, or service changed".to_string());
            return Ok(result);
        }
        let status = {
            let registry = lock_env_registry(&self.env, &self.cwd)?;
            let fresh = self
                .inspect_launcher_source(name, launcher)?
                .ok_or("source binding changed")?;
            self.admit_source_update(name, &fresh)?;
            if fresh.root != source.root || fresh.head != source.head {
                return Err("source changed before native inspection".to_string());
            }
            self.source_status(name, launcher, &source, operation, Some(&registry))?
        };
        if status
            .pointer("/update/git/artifacts/ready")
            .and_then(Value::as_bool)
            .is_none()
        {
            result.note = Some("this OpenClaw checkout does not report native source artifact readiness; its update support remains unknown and no update was attempted".to_string());
            return Ok(result);
        }
        if text(&status, "/update/git/sha") != source.head
            || status.pointer("/update/git/dirty").and_then(Value::as_bool) != Some(false)
        {
            return Err(
                "source changed or its cleanliness became unknown during native inspection"
                    .to_string(),
            );
        }
        if [
            "activeRun",
            "staleRun",
            "abandonedRun",
            "runStatusError",
            "runReconciliationError",
        ]
        .iter()
        .any(|key| status.get(*key).is_some_and(|value| !value.is_null()))
        {
            return Err("native update activity or recovery is unresolved; inspect `openclaw update status --json` and resolve it before upgrading through OCM; no checkpoint or service change was made".to_string());
        }
        let before = artifact_identity(&status);
        if status
            .pointer("/update/git/artifacts/ready")
            .and_then(Value::as_bool)
            == Some(true)
            && before.is_none()
        {
            return Err("native source readiness has no usable build identity".to_string());
        }
        let service = self.upgrade_service_status(name)?;
        if service.is_none() && !self.supervisor_service().stopped_launch_observed(name)? {
            return Err("cannot confirm the source Gateway is stopped; inspect its service and runtime state before upgrading".to_string());
        }
        let channel = text(&status, "/channel/value");
        let configured = text(&status, "/channel/config");
        let target_current = match channel.as_deref() {
            Some("dev") if matches!(configured.as_deref(), None | Some("dev")) => {
                text(&status, "/update/git/branch").as_deref() == Some("main")
                    && status
                        .pointer("/update/git/fetchOk")
                        .and_then(Value::as_bool)
                        == Some(true)
                    && text(&status, "/update/git/upstreamSha") == source.head
            }
            Some("stable" | "beta") if channel == configured => {
                text(&status, "/update/git/preferredTarget/channel") == configured
                    && text(&status, "/update/git/preferredTarget/sha") == source.head
                    && text(&status, "/update/git/preferredTarget/tag")
                        .is_some_and(|tag| !tag.is_empty())
            }
            _ => false,
        };
        let current = before.is_some()
            && target_current
            && status.pointer("/update/git/dirty").and_then(Value::as_bool) == Some(false)
            && text(&status, "/update/git/sha") == source.head
            && text(&status, "/update/git/builtSha") == source.head;
        if current
            && service.as_ref().is_none_or(|running| {
                running.running
                    && running.issue.is_none()
                    && running.binding_kind.as_deref() == Some("launcher")
                    && running.binding_name.as_deref() == Some(launcher)
                    && running.child_pid.is_some_and(|pid| {
                        self.supervisor_service()
                            .running_launch_matches(name, pid)
                            .unwrap_or(false)
                    })
                    && self
                        .verify_source_gateway(
                            name,
                            launcher,
                            &source,
                            before.as_ref().unwrap(),
                            operation,
                        )
                        .is_ok_and(|(known, _)| known)
            })
        {
            result.outcome = "up-to-date".to_string();
            result.note = Some("source target and native build are current; configuration and service were left unchanged".to_string());
            return Ok(result);
        }
        let binding = UpgradeHistoryBinding {
            kind: "launcher".to_string(),
            name: launcher.to_string(),
            openclaw_version: before.as_ref().map(|identity| identity.version.clone()),
        };
        let mut transaction = self.begin_upgrade_transaction_locked(
            name,
            UpgradeTransactionPlan {
                source: binding.clone(),
                target: binding,
            },
            &[],
            options.rollback_enabled,
            "pre-source-upgrade",
            None,
            UpgradeTimingRecorder::new(),
        )?;
        result.snapshot_id = Some(transaction.snapshot_id.clone());
        let started = transaction.timings.start();
        let mut native_invoked = false;
        let updated = (|| {
            // Binding publishers already use this registry lock. Do not hold it
            // across checkpoint or service writes, which need the same lock.
            let registry = lock_env_registry(&self.env, &self.cwd)?;
            let fresh = self
                .inspect_launcher_source(name, launcher)?
                .ok_or("source binding changed")?;
            self.admit_source_update(name, &fresh)?;
            if fresh.root != source.root || fresh.head != source.head || transaction.interrupted() {
                return Err(
                    "source changed or update was interrupted before native execution".to_string(),
                );
            }
            if !self.supervisor_service().stopped_launch_observed(name)? {
                return Err(
                    "source Gateway quiescence could not be confirmed before native execution"
                        .to_string(),
                );
            }
            native_invoked = true;
            self.with_progress(format!("Updating source checkout for {name}"), || {
                self.source_command(
                    name,
                    launcher,
                    &source,
                    &["update", "--no-restart", "--json"],
                    operation,
                    Some(&registry),
                )
            })
        })();
        if native_invoked {
            transaction.migration.status = "unknown".to_string();
            transaction.finalization.status = "unknown".to_string();
        }
        transaction.timings.finish(
            "openclaw",
            "sourceUpdate",
            "stopped",
            started,
            if updated.as_ref().is_ok_and(|output| output.status.success()) {
                "completed"
            } else {
                "failed"
            },
        );
        let mut native = Value::Null;
        let completion = (|| {
            let output = updated?;
            native = serde_json::from_str(output.stdout.trim()).map_err(|_| {
                format!(
                    "native source update did not return a readable result: {}",
                    source_failure_summary(&output)
                )
            })?;
            if text(&native, "/root")
                .and_then(|root| fs::canonicalize(root).ok())
                .as_deref()
                != Some(Path::new(&source.root))
            {
                native = Value::Null;
                return Err(format!(
                    "native source result resolved a different or unknown installation: {}",
                    source_failure_summary(&output)
                ));
            }
            let accepted = text(&native, "/status").as_deref() == Some("ok")
                || (text(&native, "/status").as_deref() == Some("skipped")
                    && text(&native, "/reason").as_deref() == Some("already-current"));
            if !output.status.success() || !accepted {
                return Err(format!(
                    "native source update failed: {}",
                    source_failure_summary(&output)
                ));
            }
            transaction.mark_post_update_completed(None);
            if transaction.interrupted() {
                return Err("source update interrupted after native execution".to_string());
            }
            let after =
                self.source_artifact_status(name, launcher, &source, operation, &transaction)?;
            let identity = artifact_identity(&after)
                .ok_or("native source artifacts could not be verified after update")?;
            if text(&native, "/mode").as_deref() != Some("git")
                || text(&native, "/after/buildId").as_deref() != Some(&identity.build_id)
                || text(&native, "/after/sha") != text(&after, "/update/git/sha")
                || text(&after, "/update/git/sha") != text(&after, "/update/git/builtSha")
            {
                return Err("native source result does not match the resulting build".to_string());
            }
            transaction.target.openclaw_version = Some(identity.version.clone());
            let (action, _) = self.reconcile_upgraded_service_locked(
                name,
                service.as_ref(),
                false,
                true,
                &mut transaction.timings,
            )?;
            result.service_action = action;
            if service.is_some() {
                let (_, note) =
                    self.verify_source_gateway(name, launcher, &source, &identity, operation)?;
                result.note = Some(note);
            }
            result.source = Some(
                self.inspect_launcher_source(name, launcher)?
                    .ok_or("source binding became unavailable after update")?,
            );
            Ok(())
        })();
        if let Err(error) = completion {
            return self.finish_failed_source_update(
                result,
                transaction,
                &native,
                native_invoked,
                before.as_ref(),
                operation,
                error,
            );
        }
        // Older OCM readers only roll back updated/switched records. They must
        // not restore an old environment over source bytes that native committed.
        result.outcome = "source-updated".to_string();
        transaction.cleanup_note = Some("Source and runtime recovery belong to the native updater; this environment checkpoint does not restore source bytes.".to_string());
        if let Err(error) = self.record_upgrade_history(&transaction, &result) {
            return self.finish_failed_source_update(
                result,
                transaction,
                &Value::Null,
                true,
                None,
                operation,
                error,
            );
        }
        if !transaction.close_interrupt_fence_for_commit() {
            return self.finish_failed_source_update(
                result,
                transaction,
                &Value::Null,
                true,
                None,
                operation,
                "source update interrupted before completion".to_string(),
            );
        }
        transaction.commit();
        Ok(result)
    }

    fn finish_failed_source_update(
        &self,
        mut result: UpgradeEnvSummary,
        mut transaction: UpgradeTransaction,
        native: &Value,
        native_invoked: bool,
        before: Option<&ArtifactIdentity>,
        operation: &EnvironmentOperationLock,
        error: String,
    ) -> Result<UpgradeEnvSummary, String> {
        let name = &result.env_name;
        let launcher = &result.binding_name;
        let source = result
            .source
            .as_ref()
            .ok_or("source update lost its source identity")?;
        let rollback = text(native, "/rollbackOutcome/status");
        let rollback_not_failed =
            matches!(rollback.as_deref(), None | Some("not-needed" | "succeeded"));
        let recovered = before
            .filter(|before| {
                if !native_invoked {
                    return true;
                }
                let retained = native
                    .pointer("/recovery/serviceRestartSafe")
                    .and_then(Value::as_bool)
                    == Some(true)
                    && text(native, "/recovery/buildId").as_deref() == Some(&before.build_id)
                    && text(native, "/recovery/version").as_deref() == Some(&before.version);
                let restored = rollback.as_deref() == Some("succeeded")
                    && native
                        .pointer("/recovery/packageRollbackVerified")
                        .and_then(Value::as_bool)
                        == Some(true)
                    && text(native, "/before/buildId").as_deref() == Some(&before.build_id)
                    && text(native, "/before/sha").as_deref() == Some(&before.commit);
                text(native, "/mode").as_deref() == Some("git")
                    && rollback_not_failed
                    && (retained || restored)
            })
            .and_then(|before| {
                self.source_artifact_status(name, launcher, source, operation, &transaction)
                    .ok()
                    .and_then(|status| artifact_identity(&status))
                    .filter(|actual| {
                        actual.build_id == before.build_id
                            && actual.version == before.version
                            && actual.commit == before.commit
                    })
            });
        let stop = || {
            let meta = self.environment_service().get(name)?;
            let service = self.service_service();
            if meta.service_running || service.status(name)?.running {
                service.stop_locked(name)?;
                service.wait_for_runtime_mutation_quiescence_locked(name)?;
            }
            Ok::<(), String>(())
        };
        let recovery: Result<String, String> = (|| {
            stop()?;
            if let Some(identity) = recovered {
                let mut note =
                    "native recovery verified; OCM restored the prior service policy".to_string();
                if transaction.service_before.enabled && transaction.service_before.running {
                    let started = self.service_service().start_locked(name)?;
                    self.wait_for_restarted_gateway_health(name, started.desired_running)?;
                    let (_, identity_note) =
                        self.verify_source_gateway(name, launcher, source, &identity, operation)?;
                    note.push_str(&format!("; {identity_note}"));
                }
                Ok(note)
            } else {
                Ok(
                    "recovery is unresolved; service remains stopped; retain the checkpoint and inspect native update recovery before restarting".to_string(),
                )
            }
        })();
        result.outcome = "failed".to_string();
        result.service_action = None;
        result.rollback = Some("native".to_string());
        let note = match recovery {
            Ok(note) => note.to_string(),
            Err(recovery_error) => {
                let stop_note = stop()
                    .err()
                    .map(|error| format!("; stopping the failed service also failed: {error}"))
                    .unwrap_or_default();
                format!("recovery and service state remain unresolved: {recovery_error}{stop_note}")
            }
        };
        transaction.cleanup_note = Some(note.clone());
        result.note = Some(format!("{note}\n{error}"));
        if let Err(history_error) = self.record_upgrade_history(&transaction, &result) {
            result.note = Some(format!(
                "{}; upgrade history was not recorded: {history_error}",
                result.note.unwrap_or_default()
            ));
        }
        transaction.finish_failed(false);
        Ok(result)
    }
}
