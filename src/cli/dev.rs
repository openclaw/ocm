use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
#[cfg(windows)]
use std::os::windows::{io::AsRawHandle, process::CommandExt as _};

use serde::Serialize;
use serde_json::Value;

use super::Cli;
use super::render::RenderProfile;
use crate::env::{
    CreateEnvironmentOptions, CreateSourceWatchOverrideOptions, EnvDevMeta, EnvMeta,
    SourceWatchCompletion, SourceWatchEndpoint, SourceWatchLease, SourceWatchSession,
    SourceWatchState,
};
use crate::infra::process::run_direct;
#[cfg(unix)]
use crate::infra::process_identity::process_group_members;
use crate::infra::process_identity::{ProcessIdentity, observe_process, process_scope_id};
use crate::infra::shell::{build_openclaw_dev_source_env, build_openclaw_env};
use crate::infra::terminal::{Cell, KeyValueRow, Tone, paint, render_key_value_card, render_table};
use crate::openclaw_repo::{
    detect_openclaw_checkout, discover_enclosing_openclaw_checkout, ensure_openclaw_worktree,
    ensure_source_dependency_install_target, inspect_source_dependencies,
    inspect_source_dependencies_with_runner, validate_openclaw_worktree,
};
use crate::service::service_backend_support_error;
use crate::store::{
    derive_env_paths, display_path, ensure_minimum_local_openclaw_config, resolve_absolute_path,
    validate_name,
};

const SOURCE_WATCH_TREE_ACTIVE_ERROR: &str = "source watch process tree is still active";
#[cfg(unix)]
const SOURCE_WATCH_NODE_SHIM: &str = r#"import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
const startFd = Number(process.env.OCM_SOURCE_WATCH_RELEASED_FD);
delete process.env.OCM_SOURCE_WATCH_RELEASED_FD;
if (Number.isInteger(startFd)) fs.closeSync(startFd);
const script = path.resolve("scripts/watch-node.mjs");
process.argv = [process.execPath, script, ...process.argv.slice(1)];
if (process.env.OCM_SOURCE_WATCH_FORCE_TTY === "1") {
  delete process.env.OCM_SOURCE_WATCH_FORCE_TTY;
  Object.defineProperty(process.stdin, "isTTY", { value: true });
}
await import(pathToFileURL(script).href);"#;

#[cfg(unix)]
const SOURCE_WATCH_SETUP_SHIM: &str = r#"case "$OCM_SOURCE_WATCH_START_FD" in
  ''|*[!0-9]*) exit 1 ;;
esac
IFS= read -r ocm_start < "/dev/fd/$OCM_SOURCE_WATCH_START_FD" || exit 1
unset OCM_SOURCE_WATCH_START_FD
exec "$@"
"#;

#[cfg(unix)]
const SOURCE_WATCH_NODE_GATE: &str = r#"case "$OCM_SOURCE_WATCH_START_FD" in
  ''|*[!0-9]*) exit 1 ;;
esac
IFS= read -r ocm_start < "/dev/fd/$OCM_SOURCE_WATCH_START_FD" || exit 1
export OCM_SOURCE_WATCH_RELEASED_FD="$OCM_SOURCE_WATCH_START_FD"
unset OCM_SOURCE_WATCH_START_FD
exec "$@"
"#;

fn source_watch_node_command(args: &[String]) -> Command {
    #[cfg(unix)]
    let mut command = {
        // Node preload hooks run before --eval, so gate the executable itself.
        // The watch shim closes the consumed descriptor before importing source.
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            SOURCE_WATCH_NODE_GATE,
            "ocm-source-watch",
            "node",
            "--input-type=module",
            "--eval",
            SOURCE_WATCH_NODE_SHIM,
        ]);
        command
    };
    #[cfg(not(unix))]
    let mut command = {
        let mut command = Command::new("node");
        command.arg("scripts/watch-node.mjs");
        command
    };
    command.args(args);
    command
}

type SourceWatchResult<T> = Result<T, SourceWatchError>;

#[derive(Debug)]
struct SourceWatchError {
    message: String,
    cleanup_verified: bool,
}

impl SourceWatchError {
    fn unverified(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cleanup_verified: false,
        }
    }
}

impl From<String> for SourceWatchError {
    fn from(message: String) -> Self {
        Self {
            message,
            cleanup_verified: true,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DevStatusSummary {
    env_name: String,
    root: String,
    repo_root: Option<String>,
    worktree_root: Option<String>,
    gateway_port: u32,
    gateway_url: String,
    gateway_port_reachable: bool,
    config_path: String,
    workspace_dir: String,
    service_enabled: bool,
    service_running: bool,
    service_desired_running: bool,
    service_pid: Option<u32>,
    source_watch: DevSourceWatchSummary,
    logs_command: String,
    status_command: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DevSourceWatchSummary {
    state: &'static str,
    pid: Option<u32>,
    #[serde(with = "time::serde::rfc3339::option")]
    started_at: Option<time::OffsetDateTime>,
    issue: Option<String>,
}

struct ExistingEnvSourceWatchOptions {
    repo_root: Option<String>,
    root: Option<String>,
    gateway_port: Option<u32>,
    watch: bool,
    force: bool,
    onboard: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DevStopSummary {
    pub(super) env_name: String,
    pub(super) stopped: bool,
    pub(super) service_restored: bool,
}

impl Cli {
    pub(super) fn handle_dev_command(&self, args: Vec<String>) -> Result<i32, String> {
        match args.first().map(String::as_str).unwrap_or("") {
            "" | "help" | "--help" | "-h" => self.dispatch_help_command(vec!["dev".to_string()]),
            "status" => self.handle_dev_status(args[1..].to_vec()),
            "stop" => self.handle_dev_stop(args[1..].to_vec()),
            _ => self.handle_dev_run(args),
        }
    }

    fn handle_dev_stop(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json, _profile) = self.consume_human_output_flags(args, "dev stop")?;
        let name = args
            .first()
            .ok_or_else(|| "dev stop requires an environment name".to_string())?;
        let name = validate_name(name, "Environment name")?;
        Self::assert_no_extra_args(&args[1..])?;
        let summary = self.stop_source_watch(&name)?;
        if json {
            self.print_json(&summary)?;
        } else if summary.stopped {
            self.stdout_line(format!("Stopped source watch for {}.", summary.env_name));
            if summary.service_restored {
                self.stdout_line(format!(
                    "Restored background service for {}.",
                    summary.env_name
                ));
            }
        } else {
            self.stdout_line(format!(
                "No active source-watch session for {}.",
                summary.env_name
            ));
        }
        Ok(0)
    }

    pub(super) fn stop_source_watch(&self, env_name: &str) -> Result<DevStopSummary, String> {
        self.stop_source_watch_generation(env_name, None)
    }

    pub(super) fn stop_source_watch_generation(
        &self,
        env_name: &str,
        expected_generation: Option<&str>,
    ) -> Result<DevStopSummary, String> {
        let env_service = self.environment_service();
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        let mut requested_lease: Option<String> = None;
        loop {
            let operation = env_service.lock_operation(env_name)?;
            let session = env_service.source_watch_session(env_name)?;
            if expected_generation.is_some_and(|expected| {
                session.as_ref().map(|session| session.lease_id.as_str()) != Some(expected)
            }) {
                return Err(
                    "source watch generation changed; the replacement session was not stopped"
                        .to_string(),
                );
            }
            if let Some(lease_id) = &requested_lease
                && let Some(completion) = env_service.source_watch_completion(env_name, lease_id)?
            {
                return dev_stop_completed(env_name, completion);
            }
            let Some(session) = session else {
                if requested_lease.is_some() {
                    return Err("source watch ownership disappeared before stop completion could be verified".to_string());
                }
                env_service.get(env_name)?;
                if !matches!(
                    env_service.observe_source_watch(env_name)?,
                    SourceWatchState::Inactive
                ) {
                    return Err("this watch session does not record stop ownership; stop it from its original terminal".to_string());
                }
                return Ok(DevStopSummary {
                    env_name: env_name.to_string(),
                    stopped: false,
                    service_restored: false,
                });
            };
            if requested_lease.is_none() && session.closed {
                if !matches!(
                    env_service.observe_source_watch(env_name)?,
                    SourceWatchState::Inactive
                ) {
                    return Err("the active watch is not owned by the completed stop session; stop it from its original terminal".to_string());
                }
                return Ok(DevStopSummary {
                    env_name: env_name.to_string(),
                    stopped: false,
                    service_restored: false,
                });
            }
            if session.process_scope != process_scope_id()? {
                return Err("source watch belongs to another boot or process namespace; no processes were signaled and service policy was preserved".to_string());
            }
            if requested_lease
                .as_ref()
                .is_some_and(|lease| lease != &session.lease_id)
            {
                return Err(
                    "source watch generation changed; the replacement session was not stopped"
                        .to_string(),
                );
            }
            if requested_lease.is_none() {
                if let Some(completion) = env_service.request_source_watch_stop_locked(&session)? {
                    return dev_stop_completed(env_name, completion);
                }
                requested_lease = Some(session.lease_id.clone());
            }
            if session.controller_is_running()? {
                #[cfg(unix)]
                if observe_process(session.controller.pid)?.is_some_and(|process| {
                    process.stopped && process.identity == session.controller
                }) {
                    signal_matching_source_watch_process(&session.controller, libc::SIGCONT)?;
                }
                drop(operation);
                if std::time::Instant::now() >= deadline {
                    return Err("source watch has not acknowledged a complete stop; its ownership and service restoration state were retained".to_string());
                }
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            let admission = env_service.lock_gateway_admission(env_name)?;
            let session = env_service.source_watch_session(env_name)?.ok_or_else(|| {
                "source watch ownership disappeared before crash recovery".to_string()
            })?;
            if requested_lease.as_deref() != Some(session.lease_id.as_str()) {
                return Err(
                    "source watch generation changed; refusing stale crash recovery".to_string(),
                );
            }
            if let Some(completion) = session.completion.clone() {
                return dev_stop_completed(env_name, completion);
            }
            if session.closed {
                return Err("source watch closed without a verified completion record".to_string());
            }
            if session.controller_is_running()? {
                return Err("source watch controller changed before crash recovery; retry stop for its current owner".to_string());
            }
            let observed = env_service
                .observe_source_watch_lease(env_name)?
                .ok_or_else(|| {
                    "source watch lease disappeared; refusing unverified crash recovery".to_string()
                })?;
            if observed.lease_id != session.lease_id {
                return Err(
                    "source watch generation changed; refusing stale crash recovery".to_string(),
                );
            }
            stop_orphaned_source_watch(&session)?;
            let mut lease = env_service.reclaim_source_watch_lease_locked(session)?;
            env_service.clear_source_watch_override_for_lease(env_name, lease.lease_id())?;
            drop(admission);
            drop(operation);
            let should_restore = lease
                .session()
                .is_some_and(|session| session.restore_service);
            let restore = if should_restore {
                self.restore_source_watch_service(env_name, &mut lease)
            } else {
                Ok(())
            };
            let restored = should_restore && restore.is_ok();
            finish_source_watch_session(env_name, &mut lease, Ok(0), restore, restored)?;
            return Ok(DevStopSummary {
                env_name: env_name.to_string(),
                stopped: true,
                service_restored: restored,
            });
        }
    }

    fn handle_dev_status(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "dev status")?;
        let target = args
            .first()
            .map(|name| validate_name(name, "Environment name"))
            .transpose()?;
        Self::assert_no_extra_args(&args[target.is_some() as usize..])?;

        let envs = match target.as_deref() {
            Some(name) => vec![self.environment_service().get(name)?],
            None => self.environment_service().list()?,
        };
        let service_pids = self
            .supervisor_service()
            .live_runtime_state()?
            .into_iter()
            .flat_map(|runtime| runtime.children)
            .map(|child| (child.env_name, child.pid))
            .collect::<BTreeMap<_, _>>();
        let mut summaries = envs
            .into_iter()
            .filter_map(|meta| {
                let service_pid = service_pids.get(&meta.name).copied();
                self.build_dev_status_summary(meta, service_pid).transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        summaries.sort_by(|left, right| left.env_name.cmp(&right.env_name));

        if let Some(target) = target {
            let summary = summaries
                .into_iter()
                .find(|summary| summary.env_name == target)
                .ok_or_else(|| format!("environment \"{target}\" is not a dev env"))?;
            if json_flag {
                self.print_json(&summary)?;
            } else {
                self.stdout_lines(render_dev_status(&summary, profile));
            }
            return Ok(0);
        }

        if json_flag {
            self.print_json(&summaries)?;
            return Ok(0);
        }

        if summaries.is_empty() {
            self.stdout_line("No dev envs.");
            return Ok(0);
        }

        self.stdout_lines(render_dev_status_list(&summaries, profile));
        Ok(0)
    }

    fn handle_dev_run(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, force) = Self::consume_flag(args, "--force");
        let (args, service_requested) = Self::consume_flag(args, "--service");
        let (args, watch) = Self::consume_flag(args, "--watch");
        let (args, onboard) = Self::consume_flag(args, "--onboard");
        let (args, repo_root) = Self::consume_option(args, "--repo")?;
        let repo_root = Self::require_option_value(repo_root, "--repo")?;
        let (args, root) = Self::consume_option(args, "--root")?;
        let root = Self::require_option_value(root, "--root")?;
        let (args, port_raw) = Self::consume_option(args, "--port")?;
        let gateway_port = match port_raw.as_deref() {
            Some(raw) => Some(Self::parse_positive_u32(raw, "--port")?),
            None => None,
        };
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;
        if force && !watch {
            return Err("dev accepts --force only with --watch".to_string());
        }
        if watch && service_requested {
            return Err("dev cannot combine --watch with --service".to_string());
        }
        if service_requested && let Some(error) = service_backend_support_error(&self.env) {
            return Err(error);
        }
        let name = validate_name(name, "Environment name")?;

        if let Some(existing) = self.environment_service().find(&name)? {
            if watch
                && (existing.dev.is_some() || force)
                && self.try_reuse_dev_watch(
                    &existing,
                    repo_root.as_deref(),
                    root.as_deref(),
                    gateway_port,
                    onboard,
                )?
            {
                return Ok(0);
            }
            if existing.dev.is_none() {
                return self.handle_existing_env_source_watch(
                    existing,
                    ExistingEnvSourceWatchOptions {
                        repo_root,
                        root,
                        gateway_port,
                        watch,
                        force,
                        onboard,
                    },
                );
            }
        }

        // Reject an already incompatible daemon before creating a new env or
        // worktree. Lease admission rechecks after any concurrent daemon change.
        if watch {
            self.supervisor_service().preflight_source_watch_daemon()?;
        }
        let (meta, created) =
            self.ensure_dev_env(&name, repo_root.clone(), root.clone(), gateway_port)?;
        let stderr_profile = self.dev_stderr_profile();
        if !watch && !service_requested && meta.service_running {
            return Err(format!(
                "dev env {} is already running in the background; stop it first with {} service stop {}, inspect it with {} logs {} --follow, or rerun with --watch --force to take it over temporarily",
                meta.name,
                self.command_example(),
                meta.name,
                self.command_example(),
                meta.name
            ));
        }
        let watch_stop = watch
            .then(install_source_watch_signal_handler)
            .transpose()?;
        let mut source_watch_lease = if watch {
            Some(
                match self
                    .environment_service()
                    .acquire_source_watch_lease(&meta.name, force)
                {
                    Ok(lease) => lease,
                    Err(error) => {
                        // A competing invocation may have claimed the lease after the first lookup.
                        let current = self.environment_service().get(&meta.name)?;
                        if self.try_reuse_dev_watch(
                            &current,
                            repo_root.as_deref(),
                            root.as_deref(),
                            gateway_port,
                            onboard,
                        )? {
                            return Ok(0);
                        }
                        return Err(error);
                    }
                },
            )
        } else {
            None
        };
        // Watch preparation belongs to the lease owner, including a newly created env.
        // A losing invocation must not rewrite config before returning the winner's status.
        let prepared = (|| {
            let current = self.environment_service().get(&meta.name)?;
            self.validate_existing_dev_request(
                &current,
                repo_root.as_deref(),
                root.as_deref(),
                gateway_port,
            )?;
            let prepared = self
                .environment_service()
                .apply_effective_gateway_port(current)?;
            self.bootstrap_dev_env(&prepared)?;
            Ok::<_, String>(prepared)
        })();
        let meta = match prepared {
            Ok(meta) => meta,
            Err(error) => {
                let result = match source_watch_lease.as_mut() {
                    Some(lease) => finish_source_watch_session(
                        &meta.name,
                        lease,
                        Err(error.into()),
                        Ok(()),
                        false,
                    ),
                    None => Err(error),
                };
                let cleanup_ready = source_watch_lease
                    .as_ref()
                    .is_none_or(|lease| lease.session().is_none());
                drop(source_watch_lease.take());
                if created && cleanup_ready {
                    let _ = self.environment_service().remove(&meta.name, true);
                }
                return result;
            }
        };
        let dev = meta
            .dev
            .as_ref()
            .ok_or_else(|| format!("environment {} is missing its dev binding", meta.name))?;
        let watch_takes_over_service = source_watch_lease
            .as_ref()
            .is_some_and(SourceWatchLease::service_was_running);
        self.stderr_lines(render_dev_run_summary(
            &meta,
            created,
            service_requested,
            watch,
            onboard,
            stderr_profile,
        ));
        self.stderr_lines(render_dev_external_plugin_warnings(
            &meta,
            Path::new(&dev.worktree_root),
            stderr_profile,
        ));

        let preparation = self.prepare_dev_run(
            &meta,
            onboard,
            source_watch_lease.as_mut(),
            watch_stop.as_deref(),
        );
        if !matches!(&preparation, Ok(0)) {
            return match source_watch_lease.as_mut() {
                Some(lease) => {
                    finish_source_watch_session(&meta.name, lease, preparation, Ok(()), false)
                }
                None => preparation.map_err(|error| error.message),
            };
        }

        if service_requested {
            self.stderr_lines(render_dev_run_step(
                "Service",
                format!(
                    "Installing and starting {} in the OCM background service",
                    meta.name
                ),
                stderr_profile,
            ));
            self.service_service().install(&meta.name)?;
            self.service_service().start(&meta.name)?;
            self.stdout_lines(render_dev_service_started(
                &meta,
                &self.command_example(),
                self.dev_stdout_profile(),
            ));
            return Ok(0);
        }

        if watch {
            if watch_takes_over_service {
                self.stderr_lines(render_dev_run_step(
                    "Takeover",
                    format!(
                        "Stopping background service for {} while watch takes over; OCM will restore it when watch exits",
                        meta.name
                    ),
                    stderr_profile,
                ));
                let lease = source_watch_lease
                    .as_mut()
                    .ok_or_else(|| "source watch lease is missing".to_string())?;
                if let Err(stop_error) = self.stop_service_for_source_watch(&meta.name, lease) {
                    let (error, restored) = self.restore_service_policy_after_failed_takeover(
                        &meta.name, stop_error, lease,
                    );
                    return finish_source_watch_session(
                        &meta.name,
                        lease,
                        Err(error.into()),
                        Ok(()),
                        restored,
                    );
                }
            }
            self.stderr_lines(render_dev_run_step(
                "Watch",
                format!(
                    "Watching {} on port {}",
                    dev.worktree_root,
                    meta.gateway_port.unwrap_or_default()
                ),
                stderr_profile,
            ));
            let watch_result = self.run_dev_gateway_watch(
                &meta,
                source_watch_lease
                    .as_mut()
                    .ok_or_else(|| "source watch lease is missing".to_string())?,
                watch_stop
                    .as_deref()
                    .ok_or_else(|| "source watch cancellation state is missing".to_string())?,
            );
            let should_restore =
                watch_takes_over_service && source_watch_allows_service_restore(&watch_result);
            let restore_result = if should_restore {
                match self.restore_source_watch_service(
                    &meta.name,
                    source_watch_lease
                        .as_mut()
                        .ok_or_else(|| "source watch lease is missing".to_string())?,
                ) {
                    Ok(()) => {
                        self.stdout_lines(render_dev_service_restored(
                            &meta,
                            &self.command_example(),
                            self.dev_stdout_profile(),
                        ));
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            } else {
                Ok(())
            };
            let restored = should_restore && restore_result.is_ok();
            return finish_source_watch_session(
                &meta.name,
                source_watch_lease
                    .as_mut()
                    .ok_or_else(|| "source watch lease is missing".to_string())?,
                watch_result,
                restore_result,
                restored,
            );
        }

        self.stderr_lines(render_dev_run_step(
            "Gateway",
            format!(
                "Starting {} on port {} from {}",
                meta.name,
                meta.gateway_port.unwrap_or_default(),
                dev.worktree_root
            ),
            stderr_profile,
        ));
        self.run_dev_gateway(&meta)
    }

    fn handle_existing_env_source_watch(
        &self,
        existing: EnvMeta,
        options: ExistingEnvSourceWatchOptions,
    ) -> Result<i32, String> {
        let ExistingEnvSourceWatchOptions {
            repo_root,
            root,
            gateway_port,
            watch,
            force,
            onboard,
        } = options;
        if !watch || !force {
            return Err(format!(
                "environment \"{}\" is not a dev env; use a new env name for `ocm dev`, or rerun with --repo <path> --watch --force to take it over temporarily",
                existing.name
            ));
        }
        if onboard {
            return Err(
                "dev takeover cannot combine --onboard with an existing non-dev env".to_string(),
            );
        }
        if root.is_some() {
            return Err("dev takeover uses the existing env root; remove --root".to_string());
        }
        if gateway_port.is_some() {
            return Err(
                "dev takeover uses the existing env gateway port; remove --port".to_string(),
            );
        }
        let Some(repo_root) = repo_root else {
            return Err(
                "dev takeover of an existing non-dev env requires --repo <path>".to_string(),
            );
        };
        let repo_root = resolve_absolute_path(&repo_root, &self.env, &self.cwd)?;
        let repo_root = detect_openclaw_checkout(&repo_root).ok_or_else(|| {
            format!(
                "OpenClaw checkout not found at {}",
                display_path(&repo_root)
            )
        })?;
        let meta = existing;
        let watch_stop = install_source_watch_signal_handler()?;
        let mut source_watch_lease = Some(
            match self
                .environment_service()
                .acquire_source_watch_lease(&meta.name, true)
            {
                Ok(lease) => lease,
                Err(error) => {
                    if self.try_reuse_dev_watch(
                        &meta,
                        Some(&display_path(&repo_root)),
                        None,
                        None,
                        false,
                    )? {
                        return Ok(0);
                    }
                    return Err(error);
                }
            },
        );
        let prepared = self
            .environment_service()
            .get(&meta.name)
            .and_then(|current| {
                self.environment_service()
                    .apply_effective_gateway_port(current)
            });
        let meta = match prepared {
            Ok(meta) => meta,
            Err(error) => {
                return finish_source_watch_session(
                    &meta.name,
                    source_watch_lease
                        .as_mut()
                        .ok_or_else(|| "source watch lease is missing".to_string())?,
                    Err(error.into()),
                    Ok(()),
                    false,
                );
            }
        };
        let lease = source_watch_lease
            .as_mut()
            .ok_or_else(|| "source watch lease is missing".to_string())?;
        let inspected = self.inspect_dev_source_dependencies(
            &repo_root,
            &build_openclaw_env(&meta, &self.env),
            true,
            Some(&mut *lease),
            Some(&watch_stop),
        );
        let preparation = if source_watch_allows_service_restore(&inspected)
            && source_watch_cancelled(lease, &watch_stop)?
        {
            Ok(130)
        } else {
            inspected.and_then(|issue| match issue {
                Some(issue) => Err(source_dependency_preparation_error(&repo_root, &issue).into()),
                None => Ok(0),
            })
        };
        if !matches!(&preparation, Ok(0)) {
            return finish_source_watch_session(&meta.name, lease, preparation, Ok(()), false);
        }
        let stderr_profile = self.dev_stderr_profile();
        self.stderr_lines(render_source_watch_takeover_summary(
            &meta,
            &repo_root,
            stderr_profile,
        ));
        self.stderr_lines(render_dev_external_plugin_warnings(
            &meta,
            &repo_root,
            stderr_profile,
        ));

        let restore_service = source_watch_lease
            .as_ref()
            .is_some_and(SourceWatchLease::service_was_running);
        if restore_service {
            self.stderr_lines(render_dev_run_step(
                "Takeover",
                format!(
                    "Stopping background service for {} while source watch takes over; OCM will restore it when watch exits",
                    meta.name
                ),
                stderr_profile,
            ));
            let lease = source_watch_lease
                .as_mut()
                .ok_or_else(|| "source watch lease is missing".to_string())?;
            if let Err(stop_error) = self.stop_service_for_source_watch(&meta.name, lease) {
                let (error, restored) = self
                    .restore_service_policy_after_failed_takeover(&meta.name, stop_error, lease);
                return finish_source_watch_session(
                    &meta.name,
                    lease,
                    Err(error.into()),
                    Ok(()),
                    restored,
                );
            }
        }

        self.stderr_lines(render_dev_run_step(
            "Watch",
            format!(
                "Watching {} on port {} for env {}",
                display_path(&repo_root),
                meta.gateway_port.unwrap_or_default(),
                meta.name
            ),
            stderr_profile,
        ));
        let watch_result = self.run_source_gateway_watch(
            &meta,
            &repo_root,
            true,
            source_watch_lease
                .as_mut()
                .ok_or_else(|| "source watch lease is missing".to_string())?,
            &watch_stop,
        );

        let should_restore = restore_service && source_watch_allows_service_restore(&watch_result);
        let restore_result = if should_restore {
            match self.restore_source_watch_service(
                &meta.name,
                source_watch_lease
                    .as_mut()
                    .ok_or_else(|| "source watch lease is missing".to_string())?,
            ) {
                Ok(()) => {
                    self.stdout_lines(render_source_watch_service_restored(
                        &meta,
                        &repo_root,
                        &self.command_example(),
                        self.dev_stdout_profile(),
                    ));
                    Ok(())
                }
                Err(error) => Err(error),
            }
        } else {
            Ok(())
        };
        let restored = should_restore && restore_result.is_ok();
        finish_source_watch_session(
            &meta.name,
            source_watch_lease
                .as_mut()
                .ok_or_else(|| "source watch lease is missing".to_string())?,
            watch_result,
            restore_result,
            restored,
        )
    }

    fn stop_service_for_source_watch(
        &self,
        env_name: &str,
        lease: &mut SourceWatchLease,
    ) -> Result<(), String> {
        let env_service = self.environment_service();
        let _operation = env_service.lock_operation(env_name)?;
        self.ensure_source_watch_env_matches(env_name, lease)?;
        lease.begin_service_takeover()?;
        let stop_result = self.service_service().stop_locked(env_name);
        match stop_result {
            Ok(summary) if !summary.running => Ok(()),
            Ok(summary) => Err(source_watch_stop_timeout_error(&summary)),
            Err(error) => Err(format!(
                "failed stopping background service for {env_name}: {error}"
            )),
        }
    }

    fn ensure_source_watch_env_matches(
        &self,
        env_name: &str,
        lease: &mut SourceWatchLease,
    ) -> Result<(), String> {
        let Some(current) = self.environment_service().find(env_name)? else {
            lease.discard_service_restore()?;
            return Err(
                "the environment no longer exists; its previous service was not restored"
                    .to_string(),
            );
        };
        if lease
            .session()
            .is_some_and(|session| !session.restore_target_matches(&current))
        {
            lease.discard_service_restore()?;
            return Err("the environment changed during source watch; its current service policy was preserved".to_string());
        }
        Ok(())
    }

    fn restore_source_watch_service(
        &self,
        env_name: &str,
        lease: &mut SourceWatchLease,
    ) -> Result<(), String> {
        let env_service = self.environment_service();
        let _operation = env_service.lock_operation(env_name)?;
        self.ensure_source_watch_env_matches(env_name, lease)?;
        lease.begin_service_restore()?;
        self.stderr_lines(render_dev_run_step(
            "Restore",
            format!("Starting background service for {env_name}"),
            self.dev_stderr_profile(),
        ));
        self.service_service()
            .start_action_locked(env_name)?
            .ensure_gateway_ready()
    }

    fn restore_service_policy_after_failed_takeover(
        &self,
        env_name: &str,
        stop_error: String,
        source_watch_lease: &mut SourceWatchLease,
    ) -> (String, bool) {
        match self.restore_source_watch_service(env_name, source_watch_lease) {
            Ok(()) => (
                format!(
                    "{stop_error}; restored the background service policy and did not start source watch"
                ),
                true,
            ),
            Err(restore_error) => (
                format!(
                    "{stop_error}; also failed restoring the background service policy: {restore_error}"
                ),
                false,
            ),
        }
    }

    fn validate_existing_dev_request(
        &self,
        existing: &EnvMeta,
        repo_root: Option<&str>,
        root: Option<&str>,
        gateway_port: Option<u32>,
    ) -> Result<(), String> {
        let dev = existing.dev.as_ref().ok_or_else(|| {
            format!(
                "environment \"{}\" is not a dev env; use a new env name for `ocm dev`",
                existing.name
            )
        })?;
        let existing_repo = PathBuf::from(&dev.repo_root);
        if let Some(repo_root) = repo_root {
            let requested = resolve_absolute_path(repo_root, &self.env, &self.cwd)?;
            let requested = fs::canonicalize(&requested).map_err(|error| {
                format!(
                    "failed to resolve OpenClaw repo {}: {error}",
                    display_path(&requested)
                )
            })?;
            let saved_repo = fs::canonicalize(&existing_repo).map_err(|error| {
                format!(
                    "failed to resolve saved OpenClaw repo {}: {error}",
                    display_path(&existing_repo)
                )
            })?;
            if requested != saved_repo {
                return Err(format!(
                    "dev cannot change the repo for existing env {}; current repo is {}",
                    existing.name, dev.repo_root
                ));
            }
        }
        if let Some(root) = root {
            let requested = resolve_absolute_path(root, &self.env, &self.cwd)?;
            let current = PathBuf::from(&existing.root);
            if requested != current {
                return Err(format!(
                    "dev cannot change the root for existing env {}; current root is {}",
                    existing.name, existing.root
                ));
            }
        }

        let (current_port, _) = self
            .environment_service()
            .resolve_effective_gateway_port(existing)?;
        if let Some(requested_port) = gateway_port
            && requested_port != current_port
        {
            return Err(format!(
                "dev cannot change the port for existing env {}; current port is {}",
                existing.name, current_port
            ));
        }
        validate_openclaw_worktree(&existing_repo, Path::new(&dev.worktree_root))?;
        Ok(())
    }

    fn try_reuse_dev_watch(
        &self,
        meta: &EnvMeta,
        repo_root: Option<&str>,
        root: Option<&str>,
        gateway_port: Option<u32>,
        onboard: bool,
    ) -> Result<bool, String> {
        let observation = self
            .environment_service()
            .observe_source_watch(&meta.name)?;
        let state = match &observation {
            SourceWatchState::Inactive => return Ok(false),
            SourceWatchState::Starting => "starting",
            SourceWatchState::Active(_) => "active",
            SourceWatchState::Restoring => "restoring",
        };
        if onboard {
            return Err(format!(
                "cannot onboard env {} while its source watch is {state}; stop the watch session first",
                meta.name
            ));
        }
        let expected_source = if let Some(dev) = &meta.dev {
            self.validate_existing_dev_request(meta, repo_root, root, None)?;
            PathBuf::from(&dev.worktree_root)
        } else {
            if root.is_some() {
                return Err("dev takeover uses the existing env root; remove --root".to_string());
            }
            if gateway_port.is_some() {
                return Err(
                    "dev takeover uses the existing env gateway port; remove --port".to_string(),
                );
            }
            let repo_root = repo_root.ok_or_else(|| {
                "dev takeover of an existing non-dev env requires --repo <path>".to_string()
            })?;
            self.resolve_dev_repo_root(Some(repo_root.to_string()))?
        };
        if let SourceWatchState::Active(active) = &observation {
            let requested = fs::canonicalize(&expected_source).map_err(|error| {
                format!(
                    "failed to resolve requested source {}: {error}",
                    display_path(&expected_source)
                )
            })?;
            let current = fs::canonicalize(&active.repo_root).map_err(|error| {
                format!(
                    "failed to resolve active source {}: {error}",
                    active.repo_root
                )
            })?;
            if requested != current {
                return Err(format!(
                    "source watch for env {} already uses {}; stop that session before selecting {}",
                    meta.name,
                    active.repo_root,
                    display_path(&requested)
                ));
            }
        }
        let SourceWatchState::Active(active) = &observation else {
            self.stderr_line(format!(
                "Source watch for {} is {state}; keeping the existing session.",
                meta.name
            ));
            self.stdout_line(format!(
                "Inspect progress with {} dev status {}.",
                self.command_example(),
                meta.name
            ));
            return Ok(true);
        };
        let endpoint = active.endpoint.as_ref().ok_or_else(|| format!(
            "source watch for env {} has no recorded launch endpoint; stop it from its original terminal and start it again before reusing it", meta.name
        ))?;
        if endpoint.env_root != meta.root {
            return Err(format!(
                "source watch for env {} was launched with root {}; the registered root changed, so the session cannot be reused",
                meta.name, endpoint.env_root
            ));
        }
        if let Some(requested_port) = gateway_port
            && requested_port != endpoint.gateway_port
        {
            return Err(format!(
                "source watch for env {} is using port {}; stop the session before selecting port {requested_port}",
                meta.name, endpoint.gateway_port
            ));
        }
        let service_pid = self
            .supervisor_service()
            .live_runtime_state()?
            .and_then(|runtime| {
                runtime
                    .children
                    .into_iter()
                    .find(|child| child.env_name == meta.name)
            })
            .map(|child| child.pid);
        let summary = self
            .build_dev_status_summary_with_watch(meta.clone(), service_pid, Ok(observation))?
            .ok_or_else(|| {
                format!(
                    "source watch for env {} disappeared during inspection",
                    meta.name
                )
            })?;
        self.stderr_line(format!(
            "Source watch for {} is {state}; keeping the existing session.",
            meta.name
        ));
        self.stdout_lines(render_dev_status(&summary, self.dev_stdout_profile()));
        Ok(true)
    }

    fn ensure_dev_env(
        &self,
        name: &str,
        repo_root: Option<String>,
        root: Option<String>,
        gateway_port: Option<u32>,
    ) -> Result<(EnvMeta, bool), String> {
        if let Some(existing) = self.environment_service().find(name)? {
            self.validate_existing_dev_request(
                &existing,
                repo_root.as_deref(),
                root.as_deref(),
                gateway_port,
            )?;
            return Ok((existing, false));
        }

        let repo_root = self.resolve_dev_repo_root(repo_root)?;
        let worktree_root = ensure_openclaw_worktree(&repo_root, name)?;

        let created = self.environment_service().create(CreateEnvironmentOptions {
            name: name.to_string(),
            root,
            gateway_port,
            service_enabled: false,
            service_running: false,
            default_runtime: None,
            default_launcher: None,
            dev: Some(EnvDevMeta {
                repo_root: display_path(&repo_root),
                worktree_root: display_path(&worktree_root),
            }),
            protected: false,
        });
        let created = match created {
            Ok(meta) => meta,
            Err(error) => {
                let _ = crate::openclaw_repo::remove_openclaw_worktree(&repo_root, &worktree_root);
                return Err(error);
            }
        };

        Ok((created, true))
    }

    fn resolve_dev_repo_root(&self, repo_root: Option<String>) -> Result<PathBuf, String> {
        let selected = match repo_root {
            Some(repo_root) => resolve_absolute_path(&repo_root, &self.env, &self.cwd)?,
            None => discover_enclosing_openclaw_checkout(&self.cwd).ok_or_else(|| {
                "OpenClaw checkout not found in the current directory or its parents; pass --repo /path/to/openclaw".to_string()
            })?,
        };
        let checkout = detect_openclaw_checkout(&selected)
            .ok_or_else(|| format!("OpenClaw checkout not found at {}", display_path(&selected)))?;
        fs::canonicalize(&checkout).map_err(|error| {
            format!(
                "failed to resolve OpenClaw checkout {}: {error}",
                display_path(&checkout)
            )
        })
    }

    fn bootstrap_dev_env(&self, meta: &EnvMeta) -> Result<(), String> {
        let paths = derive_env_paths(Path::new(&meta.root));
        let (gateway_port, _) = self
            .environment_service()
            .resolve_effective_gateway_port(meta)?;
        ensure_minimum_local_openclaw_config(&paths, gateway_port)
    }

    fn prepare_dev_run(
        &self,
        meta: &EnvMeta,
        onboard: bool,
        mut lease: Option<&mut SourceWatchLease>,
        stop: Option<&AtomicBool>,
    ) -> SourceWatchResult<i32> {
        if let (Some(lease), Some(stop)) = (lease.as_deref(), stop)
            && source_watch_cancelled(lease, stop)?
        {
            return Ok(130);
        }
        let code = self.ensure_dev_dependencies(meta, lease.as_deref_mut(), stop)?;
        if code != 0 {
            return Ok(code);
        }
        if onboard {
            let source = meta
                .dev
                .as_ref()
                .ok_or_else(|| "dev binding is missing".to_string())?;
            self.stderr_lines(render_dev_run_step(
                "Onboarding",
                format!("Running local onboarding in {}", source.worktree_root),
                self.dev_stderr_profile(),
            ));
            return self.run_dev_onboard(meta, lease, stop);
        }
        if let (Some(lease), Some(stop)) = (lease, stop)
            && source_watch_cancelled(lease, stop)?
        {
            return Ok(130);
        }
        Ok(0)
    }

    fn run_dev_setup(
        &self,
        program: &str,
        args: &[String],
        env: &std::collections::BTreeMap<String, String>,
        cwd: &Path,
        lease: Option<&mut SourceWatchLease>,
        stop: Option<&AtomicBool>,
    ) -> SourceWatchResult<i32> {
        let Some(lease) = lease else {
            return run_direct(program, args, env, cwd).map_err(SourceWatchError::from);
        };
        let stop = stop.ok_or_else(|| "source watch cancellation state is missing".to_string())?;
        if source_watch_cancelled(lease, stop)? {
            return Ok(130);
        }
        #[cfg(unix)]
        let mut command = {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", SOURCE_WATCH_SETUP_SHIM, "ocm-dev-setup", program]);
            command.args(args);
            command
        };
        #[cfg(not(unix))]
        let mut command = {
            let mut command = Command::new(program);
            command.args(args);
            command
        };
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .env_clear()
            .envs(env)
            .current_dir(cwd);
        let Some(output) = self.run_owned_source_watch_command(command, lease, stop, true)? else {
            return Ok(130);
        };
        Ok(source_watch_status_code(
            &output.status,
            stop.load(Ordering::SeqCst),
        ))
    }

    fn run_owned_source_watch_command(
        &self,
        mut command: Command,
        lease: &mut SourceWatchLease,
        stop: &AtomicBool,
        terminal: bool,
    ) -> SourceWatchResult<Option<std::process::Output>> {
        if source_watch_cancelled(lease, stop)? {
            return Ok(None);
        }
        lease.configure_child(&mut command);
        let mut guard = SourceWatchProcessGuard::new_with_terminal(terminal)?;
        guard.configure_command(&mut command)?;
        lease.begin_child_spawn()?;
        let mut child = command.spawn().map_err(|error| {
            source_watch_spawn_error(
                lease,
                format!("failed starting source watch preparation: {error}"),
            )
        })?;
        let setup = guard
            .assign_child(&child)
            .and_then(|()| lease.attach_to_child(&child))
            .and_then(|()| lease.record_child(child.id()));
        if let Err(error) = setup {
            #[cfg(windows)]
            return Err(stop_suspended_source_watch_after_error(
                &mut child, &guard, error,
            ));
            #[cfg(not(windows))]
            return Err(stop_source_watch_after_error(&mut child, &guard, error));
        }
        let stdout = child
            .stdout
            .take()
            .map(|pipe| spawn_source_capture(pipe, "stdout"));
        let stderr = child
            .stderr
            .take()
            .map(|pipe| spawn_source_capture(pipe, "stderr"));
        let start = source_watch_cancelled(lease, stop).and_then(|cancelled| {
            if cancelled {
                Ok(())
            } else {
                guard.start_child(&child)
            }
        });
        if let Err(error) = start {
            #[cfg(windows)]
            return Err(stop_suspended_source_watch_after_error(
                &mut child, &guard, error,
            ));
            #[cfg(not(windows))]
            return Err(stop_source_watch_after_error(&mut child, &guard, error));
        }
        let result = match wait_for_source_watch_child(&mut child, stop, &guard, lease) {
            Ok(status) => Ok(status),
            Err(error) => Err(stop_source_watch_after_error(
                &mut child,
                &guard,
                error.message,
            )),
        };
        let result = match (result, guard.restore_terminal()) {
            (result, Ok(())) => result,
            (Ok(_), Err(error)) => Err(error.into()),
            (Err(error), Err(terminal_error)) => Err(SourceWatchError {
                message: format!("{}; {terminal_error}", error.message),
                cleanup_verified: error.cleanup_verified,
            }),
        };
        if source_watch_allows_service_restore(&result) {
            lease.clear_child()?;
        }
        let status = result?;
        Ok(Some(std::process::Output {
            status,
            stdout: collect_source_capture(stdout)?,
            stderr: collect_source_capture(stderr)?,
        }))
    }

    fn inspect_dev_source_dependencies(
        &self,
        repo_root: &Path,
        env: &std::collections::BTreeMap<String, String>,
        watch: bool,
        lease: Option<&mut SourceWatchLease>,
        stop: Option<&AtomicBool>,
    ) -> SourceWatchResult<Option<String>> {
        match (lease, stop) {
            (Some(lease), Some(stop)) => inspect_source_dependencies_with_runner(
                repo_root,
                env,
                watch,
                cfg!(unix),
                |command| {
                    self.run_owned_source_watch_command(command, lease, stop, false)?
                        .ok_or_else(|| {
                            SourceWatchError::from(
                                "source watch preparation was cancelled".to_string(),
                            )
                        })
                },
            ),
            _ => inspect_source_dependencies(repo_root, env, watch).map_err(SourceWatchError::from),
        }
    }

    fn ensure_dev_dependencies(
        &self,
        meta: &EnvMeta,
        mut lease: Option<&mut SourceWatchLease>,
        stop: Option<&AtomicBool>,
    ) -> SourceWatchResult<i32> {
        let watch = lease.is_some();
        let dev = meta
            .dev
            .as_ref()
            .ok_or_else(|| format!("environment \"{}\" is missing its dev binding", meta.name))?;
        let worktree_root = Path::new(&dev.worktree_root);
        let process_env = build_openclaw_env(meta, &self.env);
        let inspected = self.inspect_dev_source_dependencies(
            worktree_root,
            &process_env,
            watch,
            lease.as_deref_mut(),
            stop,
        );
        if source_watch_allows_service_restore(&inspected)
            && stop.is_some_and(|stop| stop.load(Ordering::SeqCst))
        {
            return Ok(130);
        }
        if inspected?.is_none() {
            return Ok(0);
        }
        ensure_source_dependency_install_target(worktree_root, &process_env)?;

        self.stderr_lines(render_dev_run_step(
            "Dependencies",
            format!("Installing dependencies in {}", dev.worktree_root),
            self.dev_stderr_profile(),
        ));
        let code = self.run_dev_setup(
            "pnpm",
            &["install".to_string(), "--frozen-lockfile".to_string()],
            &process_env,
            worktree_root,
            lease.as_deref_mut(),
            stop,
        )?;
        if code != 0 {
            return Ok(code);
        }
        let inspected =
            self.inspect_dev_source_dependencies(worktree_root, &process_env, watch, lease, stop);
        if source_watch_allows_service_restore(&inspected)
            && stop.is_some_and(|stop| stop.load(Ordering::SeqCst))
        {
            return Ok(130);
        }
        if let Some(issue) = inspected? {
            return Err(source_dependency_preparation_error(worktree_root, &issue).into());
        }
        Ok(0)
    }

    fn dev_stderr_profile(&self) -> RenderProfile {
        let color_mode = self.color_mode();
        let pretty_enabled =
            self.stderr_is_terminal() || matches!(color_mode, super::ColorMode::Always);
        if pretty_enabled {
            RenderProfile::pretty(
                self.color_output_enabled_for(self.stderr_is_terminal(), color_mode),
            )
        } else {
            RenderProfile::raw()
        }
    }

    fn dev_stdout_profile(&self) -> RenderProfile {
        let color_mode = self.color_mode();
        let pretty_enabled =
            self.stdout_is_terminal() || matches!(color_mode, super::ColorMode::Always);
        if pretty_enabled {
            RenderProfile::pretty(
                self.color_output_enabled_for(self.stdout_is_terminal(), color_mode),
            )
        } else {
            RenderProfile::raw()
        }
    }

    fn run_dev_onboard(
        &self,
        meta: &EnvMeta,
        lease: Option<&mut SourceWatchLease>,
        stop: Option<&AtomicBool>,
    ) -> SourceWatchResult<i32> {
        let dev = meta
            .dev
            .as_ref()
            .ok_or_else(|| format!("environment \"{}\" is missing its dev binding", meta.name))?;
        let args = vec![
            "openclaw".to_string(),
            "onboard".to_string(),
            "--mode".to_string(),
            "local".to_string(),
            "--no-install-daemon".to_string(),
        ];
        self.run_dev_setup(
            "pnpm",
            &args,
            &build_openclaw_dev_source_env(meta, &self.env, Path::new(&dev.worktree_root)),
            Path::new(&dev.worktree_root),
            lease,
            stop,
        )
    }

    fn run_dev_gateway(&self, meta: &EnvMeta) -> Result<i32, String> {
        let dev = meta
            .dev
            .as_ref()
            .ok_or_else(|| format!("environment \"{}\" is missing its dev binding", meta.name))?;
        let args = vec![
            "openclaw".to_string(),
            "gateway".to_string(),
            "run".to_string(),
            "--port".to_string(),
            meta.gateway_port.unwrap_or_default().to_string(),
        ];
        run_direct(
            "pnpm",
            &args,
            &build_openclaw_dev_source_env(meta, &self.env, Path::new(&dev.worktree_root)),
            Path::new(&dev.worktree_root),
        )
    }

    fn run_dev_gateway_watch(
        &self,
        meta: &EnvMeta,
        source_watch_lease: &mut SourceWatchLease,
        stop_requested: &AtomicBool,
    ) -> SourceWatchResult<i32> {
        let dev = meta
            .dev
            .as_ref()
            .ok_or_else(|| format!("environment \"{}\" is missing its dev binding", meta.name))?;
        self.run_source_gateway_watch(
            meta,
            Path::new(&dev.worktree_root),
            false,
            source_watch_lease,
            stop_requested,
        )
    }

    fn run_source_gateway_watch(
        &self,
        meta: &EnvMeta,
        repo_root: &Path,
        tee_to_env_logs: bool,
        lease: &mut SourceWatchLease,
        stop_requested: &AtomicBool,
    ) -> SourceWatchResult<i32> {
        let args = [
            "scripts/watch-node.mjs".to_string(),
            "gateway".to_string(),
            "run".to_string(),
            "--port".to_string(),
            meta.gateway_port.unwrap_or_default().to_string(),
        ];
        if source_watch_cancelled(lease, stop_requested)? {
            return Ok(130);
        }

        let mut command = source_watch_node_command(&args[1..]);
        #[cfg(unix)]
        let source_watch_force_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } != 1;
        command
            .stdin(Stdio::inherit())
            .env_clear()
            .envs(build_openclaw_dev_source_env(meta, &self.env, repo_root))
            .current_dir(repo_root);
        #[cfg(unix)]
        if source_watch_force_tty {
            // watch-node detaches its runner solely from stdin.isTTY. Override that decision
            // while preserving the real noninteractive stdin and EOF inherited by the runner.
            command.env("OCM_SOURCE_WATCH_FORCE_TTY", "1");
        }
        lease.configure_child(&mut command);

        let mut log_files = if tee_to_env_logs {
            Some(open_source_watch_log_files(meta)?)
        } else {
            None
        };

        if log_files.is_some() {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
        } else {
            command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        }

        let mut process_guard = SourceWatchProcessGuard::new()?;
        process_guard.configure_command(&mut command)?;
        lease.begin_child_spawn()?;
        let mut child = command.spawn().map_err(|error| {
            source_watch_spawn_error(lease, format!("failed to run \"node\": {error}"))
        })?;
        if let Err(error) = process_guard.assign_child(&child) {
            #[cfg(windows)]
            return Err(stop_suspended_source_watch_after_error(
                &mut child,
                &process_guard,
                error,
            ));
            #[cfg(not(windows))]
            return Err(stop_source_watch_after_error(
                &mut child,
                &process_guard,
                error,
            ));
        }
        if let Err(error) = lease.attach_to_child(&child) {
            #[cfg(windows)]
            return Err(stop_suspended_source_watch_after_error(
                &mut child,
                &process_guard,
                error,
            ));
            #[cfg(not(windows))]
            return Err(stop_source_watch_after_error(
                &mut child,
                &process_guard,
                error,
            ));
        }
        if let Err(error) = lease.record_child(child.id()) {
            #[cfg(windows)]
            return Err(stop_suspended_source_watch_after_error(
                &mut child,
                &process_guard,
                error,
            ));
            #[cfg(not(windows))]
            return Err(stop_source_watch_after_error(
                &mut child,
                &process_guard,
                error,
            ));
        }
        let source_watch = match self
            .environment_service()
            .create_source_watch_override_with_lease(
                CreateSourceWatchOverrideOptions {
                    env_name: meta.name.clone(),
                    repo_root: repo_root.to_path_buf(),
                    endpoint: SourceWatchEndpoint {
                        env_root: meta.root.clone(),
                        gateway_port: meta.gateway_port.unwrap_or_default(),
                    },
                    watch_pid: child.id(),
                },
                lease,
            ) {
            Ok(source_watch) => source_watch,
            Err(error) => {
                return Err(stop_source_watch_after_error(
                    &mut child,
                    &process_guard,
                    error,
                ));
            }
        };
        let start_result = source_watch_cancelled(lease, stop_requested).and_then(|cancelled| {
            if cancelled {
                Ok(())
            } else {
                process_guard.start_child(&child)
            }
        });
        if let Err(error) = start_result {
            #[cfg(windows)]
            return Err(stop_suspended_source_watch_after_error(
                &mut child,
                &process_guard,
                error,
            ));
            #[cfg(not(windows))]
            return Err(stop_source_watch_after_error(
                &mut child,
                &process_guard,
                error,
            ));
        }
        let mut tee_threads = Vec::new();
        if let Some(log_files) = log_files.take() {
            let Some(stdout) = child.stdout.take() else {
                let _ = self
                    .environment_service()
                    .clear_source_watch_override(&meta.name, &source_watch.token);
                return Err(stop_source_watch_after_error(
                    &mut child,
                    &process_guard,
                    "failed to capture source watch stdout".to_string(),
                ));
            };
            let Some(stderr) = child.stderr.take() else {
                let _ = self
                    .environment_service()
                    .clear_source_watch_override(&meta.name, &source_watch.token);
                return Err(stop_source_watch_after_error(
                    &mut child,
                    &process_guard,
                    "failed to capture source watch stderr".to_string(),
                ));
            };
            tee_threads.push(spawn_tee_thread(
                stdout,
                io::stdout(),
                log_files.stdout,
                "stdout",
            ));
            tee_threads.push(spawn_tee_thread(
                stderr,
                io::stderr(),
                log_files.stderr,
                "stderr",
            ));
        }

        let status_result =
            match wait_for_source_watch_child(&mut child, stop_requested, &process_guard, lease) {
                Ok(status) => Ok(status),
                Err(error) => Err(stop_source_watch_after_error(
                    &mut child,
                    &process_guard,
                    error.message,
                )),
            };
        let status_result = match (status_result, process_guard.restore_terminal()) {
            (result, Ok(())) => result,
            (Ok(_), Err(error)) => Err(SourceWatchError::from(error)),
            (Err(error), Err(restore_error)) => Err(SourceWatchError {
                message: format!("{}; {restore_error}", error.message),
                cleanup_verified: error.cleanup_verified,
            }),
        };
        let tee_result = if !source_watch_allows_service_restore(&status_result) {
            drop(tee_threads);
            Ok(())
        } else {
            wait_for_tee_threads(tee_threads)
        };
        let clear_result = if source_watch_allows_override_clear(&status_result) {
            self.environment_service()
                .clear_source_watch_override(&meta.name, &source_watch.token)
                .map(|_| ())
        } else {
            Ok(())
        };

        let result = combine_source_watch_cleanup_results(status_result, tee_result, clear_result);
        if source_watch_allows_service_restore(&result) {
            lease.clear_child()?;
        }
        let status = result?;
        Ok(source_watch_status_code(
            &status,
            stop_requested.load(Ordering::SeqCst),
        ))
    }

    fn build_dev_status_summary(
        &self,
        meta: EnvMeta,
        service_pid: Option<u32>,
    ) -> Result<Option<DevStatusSummary>, String> {
        let observation = self.environment_service().observe_source_watch(&meta.name);
        self.build_dev_status_summary_with_watch(meta, service_pid, observation)
    }

    fn build_dev_status_summary_with_watch(
        &self,
        meta: EnvMeta,
        service_pid: Option<u32>,
        observation: Result<SourceWatchState, String>,
    ) -> Result<Option<DevStatusSummary>, String> {
        if meta.dev.is_none() && matches!(observation, Ok(SourceWatchState::Inactive)) {
            return Ok(None);
        }
        let mut source_watch = DevSourceWatchSummary {
            state: "inactive",
            pid: None,
            started_at: None,
            issue: None,
        };
        let mut active_endpoint = None;
        let active_source = match observation {
            Ok(SourceWatchState::Inactive) => None,
            Ok(SourceWatchState::Starting) => {
                source_watch.state = "starting";
                None
            }
            Ok(SourceWatchState::Restoring) => {
                source_watch.state = "restoring";
                None
            }
            Ok(SourceWatchState::Active(watch)) => {
                source_watch.state = "active";
                source_watch.pid = Some(watch.watch_pid);
                source_watch.started_at = Some(watch.started_at);
                if watch.endpoint.is_none() {
                    source_watch.issue = Some("This watch has no recorded launch endpoint; the displayed URL comes from current configuration.".to_string());
                }
                active_endpoint = watch.endpoint;
                Some(watch.repo_root)
            }
            Err(error) => {
                source_watch.state = "unknown";
                source_watch.issue = Some(error);
                None
            }
        };
        let dev = meta.dev.as_ref();
        let gateway_port = match &active_endpoint {
            Some(endpoint) => endpoint.gateway_port,
            None => {
                self.environment_service()
                    .resolve_effective_gateway_port(&meta)?
                    .0
            }
        };
        let active_root = active_endpoint
            .as_ref()
            .map(|endpoint| endpoint.env_root.as_str())
            .unwrap_or(&meta.root);
        if active_root != meta.root {
            source_watch.issue = Some(
                "The registered env root differs from the active source watch root.".to_string(),
            );
        }
        let paths = derive_env_paths(Path::new(active_root));
        let root = active_root.to_string();
        let env_name = meta.name.clone();
        Ok(Some(DevStatusSummary {
            env_name: env_name.clone(),
            root,
            repo_root: dev
                .map(|dev| dev.repo_root.clone())
                .or_else(|| active_source.clone()),
            worktree_root: active_source.or_else(|| dev.map(|dev| dev.worktree_root.clone())),
            gateway_port,
            gateway_url: dev_gateway_url(gateway_port),
            gateway_port_reachable: crate::service::inspect::tcp_port_reachable(gateway_port),
            config_path: display_path(&paths.config_path),
            workspace_dir: display_path(&paths.workspace_dir),
            service_enabled: meta.service_enabled,
            service_running: service_pid.is_some(),
            service_desired_running: meta.service_running,
            service_pid,
            source_watch,
            logs_command: format!("{} logs {} --follow", self.command_example(), env_name),
            status_command: format!("{} dev status {}", self.command_example(), env_name),
        }))
    }
}

fn source_dependency_preparation_error(repo_root: &Path, issue: &str) -> String {
    format!(
        "OpenClaw source dependencies are not ready in {}: {issue}. Run `pnpm install --frozen-lockfile` in that checkout before retrying",
        display_path(repo_root)
    )
}

fn install_source_watch_signal_handler() -> Result<Arc<AtomicBool>, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let signal_flag = Arc::clone(&stop);
    ctrlc::set_handler(move || signal_flag.store(true, Ordering::SeqCst))
        .map_err(|error| format!("failed to install dev watch signal handler: {error}"))?;
    Ok(stop)
}

fn source_watch_status_code(status: &std::process::ExitStatus, cancelled: bool) -> i32 {
    let mut code = status.code();
    #[cfg(unix)]
    if code.is_none() {
        code = status.signal().map(|signal| 128 + signal);
    }
    source_watch_exit_code(code, cancelled)
}

fn source_watch_spawn_error(lease: &mut SourceWatchLease, error: String) -> SourceWatchError {
    match lease.clear_child() {
        Ok(()) => error.into(),
        Err(cleanup) => {
            format!("{error}; failed clearing unstarted child ownership: {cleanup}").into()
        }
    }
}

fn spawn_source_capture<R: Read + Send + 'static>(
    mut pipe: R,
    stream: &'static str,
) -> JoinHandle<Result<Vec<u8>, String>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)
            .map_err(|error| format!("failed reading source prerequisite {stream}: {error}"))?;
        Ok(bytes)
    })
}

fn collect_source_capture(
    capture: Option<JoinHandle<Result<Vec<u8>, String>>>,
) -> Result<Vec<u8>, String> {
    match capture {
        Some(capture) => capture
            .join()
            .map_err(|_| "source prerequisite output reader panicked".to_string())?,
        None => Ok(Vec::new()),
    }
}

fn dev_stop_completed(
    env_name: &str,
    completion: SourceWatchCompletion,
) -> Result<DevStopSummary, String> {
    if let Some(error) = completion.error {
        return Err(error);
    }
    Ok(DevStopSummary {
        env_name: env_name.to_string(),
        stopped: true,
        service_restored: completion.service_restored,
    })
}

#[cfg(unix)]
fn signal_matching_source_watch_process(
    expected: &ProcessIdentity,
    signal: i32,
) -> Result<bool, String> {
    if expected.pid == 0 || expected.pid > i32::MAX as u32 {
        return Err("invalid source watch process range; no signal was sent".to_string());
    }
    #[cfg(target_os = "linux")]
    let pidfd = {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, expected.pid as libc::pid_t, 0) };
        if fd == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(false);
            }
            return Err(format!(
                "failed obtaining a PID-safe source watch handle: {error}"
            ));
        }
        unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) }
    };
    if !observe_process(expected.pid)?
        .is_some_and(|process| process.running && process.identity == *expected)
    {
        return Ok(false);
    }
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    #[cfg(not(target_os = "linux"))]
    let result = unsafe { libc::kill(expected.pid as libc::pid_t, signal) } as libc::c_long;
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(format!(
            "failed signaling the recorded source watch process: {error}"
        ))
    }
}

fn stop_orphaned_source_watch(session: &SourceWatchSession) -> Result<(), String> {
    if session.process_scope != process_scope_id()? {
        return Err("source watch process scope changed; no processes were signaled".to_string());
    }
    #[cfg(windows)]
    if session.child_spawn_pending {
        return Err("source watch controller exited before publishing child ownership; Windows crash cleanup cannot be verified, so the unfinished session was retained".to_string());
    }
    let Some(child) = &session.child else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        let process = observe_process(child.pid)?;
        if !process
            .as_ref()
            .is_some_and(|process| process.running && process.identity == *child)
        {
            if process_group_members(child.pid)?.is_empty() {
                return Ok(());
            }
            return Err("recorded source watch identity no longer matches a live group owner; no processes were signaled".to_string());
        }
        if process.as_ref().and_then(|process| process.process_group) != Some(child.pid) {
            return Err(
                "recorded source watch process changed groups; no processes were signaled"
                    .to_string(),
            );
        }
        struct ResumeOnError<'a> {
            child: &'a ProcessIdentity,
            armed: bool,
        }
        impl Drop for ResumeOnError<'_> {
            fn drop(&mut self) {
                if self.armed {
                    let _ = signal_matching_source_watch_process(self.child, libc::SIGCONT);
                }
            }
        }
        if !signal_matching_source_watch_process(child, libc::SIGSTOP)? {
            return Err("source watch owner exited before its group could be stopped".to_string());
        }
        let mut paused = ResumeOnError { child, armed: true };
        let owns_group = || -> Result<bool, String> {
            Ok(observe_process(child.pid)?.is_some_and(|process| {
                process.running
                    && process.identity == *child
                    && process.process_group == Some(child.pid)
                    && process.stopped
            }))
        };
        let pause_deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !owns_group()? {
            if std::time::Instant::now() >= pause_deadline {
                return Err("source watch group owner did not acknowledge suspension".to_string());
            }
            if !observe_process(child.pid)?.is_some_and(|process| {
                process.running
                    && process.identity == *child
                    && process.process_group == Some(child.pid)
            }) {
                return Err("source watch group ownership changed during shutdown".to_string());
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Keep the recorded leader paused as the group identity anchor. A
        // default fatal signal can terminate a stopped process on Darwin, so
        // only descendants receive TERM before the final group cleanup.
        for process in process_group_members(child.pid)? {
            if process.identity.pid == child.pid {
                continue;
            }
            signal_matching_source_watch_process(&process.identity, libc::SIGTERM)?;
            if process.stopped {
                signal_matching_source_watch_process(&process.identity, libc::SIGCONT)?;
            }
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(7);
        while std::time::Instant::now() < deadline {
            if !process_group_members(child.pid)?
                .iter()
                .any(|process| process.identity.pid != child.pid)
            {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        if !owns_group()? {
            return Err("source watch group ownership changed before final cleanup".to_string());
        }
        signal_unix_process_group(child.pid, libc::SIGSTOP)?;
        signal_unix_process_group(child.pid, libc::SIGKILL)?;
        paused.armed = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !process_group_members(child.pid)?.is_empty() {
            if std::time::Instant::now() >= deadline {
                return Err(
                    "source watch process group remained active; service restoration was deferred"
                        .to_string(),
                );
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        // The existing kill-on-close job contains all children when its
        // controller exits. Never replace that authority with a PID tree scan.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match observe_process(child.pid)? {
                Some(process) if process.identity != *child => {
                    return Err(
                        "source watch PID was reused; no process was terminated".to_string()
                    );
                }
                Some(process) if process.running => {}
                _ => return Ok(()),
            }
            if std::time::Instant::now() >= deadline {
                return Err("source watch process job has not finished stopping".to_string());
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
    #[cfg(not(any(unix, windows)))]
    Err("source watch crash recovery is unsupported on this platform".to_string())
}

fn source_watch_cancelled(lease: &SourceWatchLease, stop: &AtomicBool) -> Result<bool, String> {
    if stop.load(Ordering::SeqCst) || lease.stop_requested()? {
        stop.store(true, Ordering::SeqCst);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn finish_source_watch_session(
    env_name: &str,
    lease: &mut SourceWatchLease,
    watch_result: SourceWatchResult<i32>,
    restore_result: Result<(), String>,
    service_restored: bool,
) -> Result<i32, String> {
    let preserve_session = !source_watch_allows_service_restore(&watch_result)
        || (lease
            .session()
            .is_some_and(|session| session.restore_service)
            && !service_restored);
    let result = combine_watch_and_restore_results(watch_result, restore_result, env_name);
    match lease.finish_session(
        service_restored,
        result.as_ref().err().cloned(),
        preserve_session,
    ) {
        Ok(()) => result,
        Err(error) => Err(match result {
            Ok(_) => format!("source watch ended, but its session cleanup failed: {error}"),
            Err(primary) => format!("{primary}; source watch session cleanup also failed: {error}"),
        }),
    }
}

fn source_watch_stop_timeout_error(summary: &crate::service::ServiceActionSummary) -> String {
    let warnings = if summary.warnings.is_empty() {
        String::new()
    } else {
        format!(" ({})", summary.warnings.join("; "))
    };
    format!(
        "background service for {} is still running after the stop request{warnings}",
        summary.env_name
    )
}

fn combine_watch_and_restore_results(
    watch_result: SourceWatchResult<i32>,
    restore_result: Result<(), String>,
    env_name: &str,
) -> Result<i32, String> {
    match (watch_result, restore_result) {
        (Ok(code), Ok(())) => Ok(code),
        (Err(watch_error), Ok(())) => Err(watch_error.message),
        (Ok(_), Err(restore_error)) => Err(format!(
            "source watch ended, but failed restoring background service for {env_name}: {restore_error}"
        )),
        (Err(watch_error), Err(restore_error)) => Err(format!(
            "{}; also failed restoring background service for {env_name}: {restore_error}",
            watch_error.message
        )),
    }
}

fn combine_source_watch_cleanup_results(
    status_result: SourceWatchResult<std::process::ExitStatus>,
    tee_result: Result<(), String>,
    clear_result: Result<(), String>,
) -> SourceWatchResult<std::process::ExitStatus> {
    let mut errors = Vec::new();
    let mut cleanup_verified = true;
    let status = match status_result {
        Ok(status) => Some(status),
        Err(error) => {
            cleanup_verified = error.cleanup_verified;
            errors.push(error.message);
            None
        }
    };
    if let Err(error) = tee_result {
        errors.push(error);
    }
    if let Err(error) = clear_result {
        errors.push(error);
    }
    if errors.is_empty() {
        status.ok_or_else(|| {
            SourceWatchError::from("source watch ended without an exit status".to_string())
        })
    } else {
        Err(SourceWatchError {
            message: errors.join("; "),
            cleanup_verified,
        })
    }
}

fn wait_for_source_watch_child(
    child: &mut std::process::Child,
    stop_requested: &AtomicBool,
    process_guard: &SourceWatchProcessGuard,
    lease: &SourceWatchLease,
) -> SourceWatchResult<std::process::ExitStatus> {
    loop {
        if let Some(status) = poll_source_watch_child(child, process_guard)? {
            return Ok(status);
        }
        if source_watch_cancelled(lease, stop_requested).map_err(SourceWatchError::unverified)? {
            return stop_source_watch_child(child, process_guard);
        }
        #[cfg(unix)]
        if observe_process(child.id())
            .map_err(SourceWatchError::unverified)?
            .is_some_and(|process| process.stopped)
        {
            process_guard
                .suspend_with_child(child.id(), lease, stop_requested)
                .map_err(SourceWatchError::unverified)?;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn poll_source_watch_child(
    child: &mut std::process::Child,
    guard: &SourceWatchProcessGuard,
) -> SourceWatchResult<Option<std::process::ExitStatus>> {
    #[cfg(unix)]
    {
        // Keep the exited leader waitable until group cleanup is complete, so
        // its PID cannot be reused by an unrelated process group.
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if waited == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(SourceWatchError::unverified(format!(
                "failed observing source watch exit: {error}"
            )));
        }
        // Darwin can report a stopped child despite WEXITED. Do not turn a
        // job-control transition into process-group cleanup.
        if unsafe { info.si_pid() } != child.id() as libc::pid_t
            || !matches!(
                info.si_code,
                libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
            )
        {
            return Ok(None);
        }
        guard
            .stop_remaining(child.id())
            .map_err(SourceWatchError::unverified)?;
        child.wait().map(Some).map_err(|error| {
            SourceWatchError::unverified(format!("failed reaping source watch: {error}"))
        })
    }
    #[cfg(not(unix))]
    {
        let status = child.try_wait().map_err(|error| {
            SourceWatchError::unverified(format!("failed waiting for source watch: {error}"))
        })?;
        if status.is_some() {
            guard
                .stop_remaining(child.id())
                .map_err(SourceWatchError::unverified)?;
        }
        Ok(status)
    }
}

struct SourceWatchProcessGuard {
    #[cfg(unix)]
    terminal: Option<SourceWatchTerminalGuard>,
    #[cfg(unix)]
    startup_reader: io::PipeReader,
    #[cfg(unix)]
    startup_writer: io::PipeWriter,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(unix)]
struct SourceWatchTerminalGuard {
    parent_process_group: libc::pid_t,
    foreground_was_owned: bool,
    foreground_assigned: AtomicBool,
}

impl SourceWatchProcessGuard {
    fn new() -> Result<Self, String> {
        Self::new_with_terminal(true)
    }

    fn new_with_terminal(terminal: bool) -> Result<Self, String> {
        #[cfg(not(unix))]
        let _ = terminal;
        #[cfg(unix)]
        {
            let (startup_reader, startup_writer) = source_watch_startup_pair()?;
            let terminal = if terminal && unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
                let foreground_process_group = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
                if foreground_process_group == -1 {
                    return Err(format!(
                        "failed reading source watch terminal ownership: {}",
                        io::Error::last_os_error()
                    ));
                }
                let parent_process_group = unsafe { libc::getpgrp() };
                Some(SourceWatchTerminalGuard {
                    parent_process_group,
                    foreground_was_owned: foreground_process_group == parent_process_group,
                    foreground_assigned: AtomicBool::new(false),
                })
            } else {
                None
            };
            Ok(Self {
                terminal,
                startup_reader,
                startup_writer,
            })
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::JobObjects::{
                CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            };

            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(format!(
                    "failed creating source watch process job: {}",
                    io::Error::last_os_error()
                ));
            }
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    std::mem::size_of_val(&info) as u32,
                )
            };
            if configured == 0 {
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(job);
                }
                return Err(format!(
                    "failed configuring source watch process job: {}",
                    io::Error::last_os_error()
                ));
            }
            Ok(Self { job })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {})
        }
    }

    fn configure_command(&mut self, command: &mut Command) -> Result<(), String> {
        #[cfg(unix)]
        {
            let startup_fd = self.startup_reader.as_raw_fd();
            command.env("OCM_SOURCE_WATCH_START_FD", startup_fd.to_string());
            unsafe {
                command.pre_exec(move || {
                    let flags = libc::fcntl(startup_fd, libc::F_GETFD);
                    if flags == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::fcntl(startup_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
        }
        #[cfg(not(any(unix, windows)))]
        let _ = command;
        Ok(())
    }

    fn assign_child(&self, child: &std::process::Child) -> Result<(), String> {
        #[cfg(windows)]
        {
            let assigned = unsafe {
                windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                    self.job,
                    child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE,
                )
            };
            if assigned == 0 {
                return Err(format!(
                    "failed assigning source watch to process job: {}",
                    io::Error::last_os_error()
                ));
            }
        }
        #[cfg(unix)]
        if let Some(terminal) = &self.terminal
            && terminal.foreground_was_owned
        {
            set_terminal_foreground_process_group(child.id() as libc::pid_t)?;
            terminal.foreground_assigned.store(true, Ordering::SeqCst);
        }
        #[cfg(not(any(unix, windows)))]
        let _ = child;
        Ok(())
    }

    fn start_child(&self, _child: &std::process::Child) -> Result<(), String> {
        #[cfg(unix)]
        {
            let mut writer = &self.startup_writer;
            writer
                .write_all(b"1\n")
                .map_err(|error| format!("failed releasing source watch startup gate: {error}"))?;
        }
        #[cfg(windows)]
        resume_windows_process(_child.id())?;
        Ok(())
    }

    fn restore_terminal(&self) -> Result<(), String> {
        #[cfg(unix)]
        if let Some(terminal) = &self.terminal
            && terminal.foreground_assigned.swap(false, Ordering::SeqCst)
        {
            set_terminal_foreground_process_group(terminal.parent_process_group)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn suspend_with_child(
        &self,
        child_pid: u32,
        lease: &SourceWatchLease,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        self.restore_terminal()?;
        loop {
            if source_watch_cancelled(lease, stop)? {
                break;
            }
            if unsafe { libc::kill(libc::getpid(), libc::SIGSTOP) } == -1 {
                return Err(format!(
                    "failed suspending OCM with source watch: {}",
                    io::Error::last_os_error()
                ));
            }
            if source_watch_cancelled(lease, stop)? {
                break;
            }
            let Some(terminal) = &self.terminal else {
                break;
            };
            let foreground_process_group = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
            if foreground_process_group == -1 {
                return Err(format!(
                    "failed reading resumed source watch terminal ownership: {}",
                    io::Error::last_os_error()
                ));
            }
            if foreground_process_group == terminal.parent_process_group {
                set_terminal_foreground_process_group(child_pid as libc::pid_t)?;
                terminal.foreground_assigned.store(true, Ordering::SeqCst);
                break;
            }
        }
        signal_unix_process_group(child_pid, libc::SIGCONT)?;
        Ok(())
    }

    fn stop_remaining(&self, root_pid: u32) -> Result<(), String> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;

            if unsafe { TerminateJobObject(self.job, 1) } == 0 {
                return Err(format!(
                    "{SOURCE_WATCH_TREE_ACTIVE_ERROR}; failed terminating the process job: {}; the background service was not restored",
                    io::Error::last_os_error()
                ));
            }
            return self.wait_for_windows_job_to_stop();
        }
        #[cfg(unix)]
        {
            let _ = signal_unix_process_group_for_cleanup(root_pid, libc::SIGKILL)?;
            wait_for_unix_process_group_to_stop(root_pid)
        }
        #[cfg(not(any(unix, windows)))]
        let _ = root_pid;
        #[cfg(not(any(unix, windows)))]
        Ok(())
    }

    #[cfg(windows)]
    fn wait_for_windows_job_to_stop(&self) -> Result<(), String> {
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
            QueryInformationJobObject,
        };

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            let queried = unsafe {
                QueryInformationJobObject(
                    self.job,
                    JobObjectBasicAccountingInformation,
                    std::ptr::from_mut(&mut info).cast(),
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                )
            };
            if queried == 0 {
                return Err(format!(
                    "{SOURCE_WATCH_TREE_ACTIVE_ERROR}; failed querying the process job: {}; the background service was not restored",
                    io::Error::last_os_error()
                ));
            }
            if info.ActiveProcesses == 0 {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "{SOURCE_WATCH_TREE_ACTIVE_ERROR}; the Windows process job still has {} active processes; the background service was not restored",
                    info.ActiveProcesses
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}

#[cfg(unix)]
fn source_watch_startup_pair() -> Result<(io::PipeReader, io::PipeWriter), String> {
    fn above_stdio<T: AsRawFd + FromRawFd>(stream: T) -> Result<T, String> {
        if stream.as_raw_fd() > libc::STDERR_FILENO {
            return Ok(stream);
        }
        let fd = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if fd == -1 {
            return Err(format!(
                "failed reserving source watch startup descriptor: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(unsafe { T::from_raw_fd(fd) })
    }
    // Shells can reopen a pipe through /dev/fd even when its number exceeds 9.
    let (reader, writer) = io::pipe()
        .map_err(|error| format!("failed creating source watch startup gate: {error}"))?;
    Ok((above_stdio(reader)?, above_stdio(writer)?))
}

#[cfg(unix)]
impl Drop for SourceWatchProcessGuard {
    fn drop(&mut self) {
        let _ = self.restore_terminal();
    }
}

#[cfg(windows)]
impl Drop for SourceWatchProcessGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
    }
}

#[cfg(unix)]
fn set_terminal_foreground_process_group(process_group: libc::pid_t) -> Result<(), String> {
    let mut previous_mask = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    let mut blocked_mask = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    unsafe {
        libc::sigemptyset(&mut blocked_mask);
        libc::sigaddset(&mut blocked_mask, libc::SIGTTOU);
    }
    let mask_result =
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &blocked_mask, &mut previous_mask) };
    if mask_result != 0 {
        return Err(format!(
            "failed blocking terminal ownership signal: {}",
            io::Error::from_raw_os_error(mask_result)
        ));
    }
    let terminal_result = unsafe { libc::tcsetpgrp(libc::STDIN_FILENO, process_group) };
    let terminal_error = (terminal_result == -1).then(io::Error::last_os_error);
    let restore_mask_result =
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous_mask, std::ptr::null_mut()) };
    if restore_mask_result != 0 {
        return Err(format!(
            "failed restoring terminal ownership signal mask: {}",
            io::Error::from_raw_os_error(restore_mask_result)
        ));
    }
    if let Some(error) = terminal_error {
        return Err(format!(
            "failed assigning source watch terminal ownership: {error}"
        ));
    }
    Ok(())
}

fn stop_source_watch_child(
    child: &mut std::process::Child,
    process_guard: &SourceWatchProcessGuard,
) -> SourceWatchResult<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        // watch-node forwards TERM to its runner; allow that grace before
        // enforcing the process-group ownership boundary.
        if signal_unix_process(child.id(), libc::SIGTERM).map_err(SourceWatchError::unverified)? {
            signal_unix_process_group_for_cleanup(child.id(), libc::SIGCONT)
                .map_err(SourceWatchError::unverified)?;
            let deadline = std::time::Instant::now() + Duration::from_secs(7);
            while std::time::Instant::now() < deadline {
                if let Some(status) = poll_source_watch_child(child, process_guard)? {
                    return Ok(status);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
        signal_unix_process_group_for_cleanup(child.id(), libc::SIGSTOP)
            .map_err(SourceWatchError::unverified)?;
        signal_unix_process_group_for_cleanup(child.id(), libc::SIGKILL)
            .map_err(SourceWatchError::unverified)?;
        wait_for_unix_process_group_to_stop(child.id()).map_err(SourceWatchError::unverified)?;
        let status = child.wait().map_err(|error| {
            SourceWatchError::unverified(format!(
                "failed waiting for stopped source watch: {error}"
            ))
        })?;
        Ok(status)
    }

    #[cfg(windows)]
    {
        process_guard
            .stop_remaining(child.id())
            .map_err(SourceWatchError::unverified)?;
        child.wait().map_err(|error| {
            SourceWatchError::unverified(format!(
                "failed waiting for stopped source watch: {error}"
            ))
        })
    }

    #[cfg(not(any(unix, windows)))]
    {
        child.kill().map_err(|error| {
            SourceWatchError::unverified(format!("failed stopping source watch: {error}"))
        })?;
        child.wait().map_err(|error| {
            SourceWatchError::unverified(format!(
                "failed waiting for stopped source watch: {error}"
            ))
        })
    }
}

#[cfg(unix)]
fn wait_for_unix_process_group_to_stop(process_group: u32) -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if !unix_process_group_has_live_members(process_group)? {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "{SOURCE_WATCH_TREE_ACTIVE_ERROR}; process group {process_group} is still active; the background service was not restored"
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(target_os = "linux")]
fn unix_process_group_has_live_members(process_group: u32) -> Result<bool, String> {
    for entry in fs::read_dir("/proc")
        .map_err(|error| format!("failed reading /proc for source watch processes: {error}"))?
    {
        let entry = entry.map_err(|error| {
            format!("failed reading /proc entry for source watch processes: {error}")
        })?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
        {
            continue;
        }
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            continue;
        };
        let fields = fields.split_whitespace().collect::<Vec<_>>();
        let is_zombie = matches!(fields.first().copied(), Some("Z" | "X"));
        let in_group =
            fields.get(2).and_then(|value| value.parse::<u32>().ok()) == Some(process_group);
        if in_group && !is_zombie {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
fn unix_process_group_has_live_members(process_group: u32) -> Result<bool, String> {
    let capacity = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if capacity <= 0 {
        return Err(format!(
            "failed listing source watch processes: {}",
            io::Error::last_os_error()
        ));
    }
    let mut pids = vec![0_i32; capacity as usize];
    let count = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    if count < 0 {
        return Err(format!(
            "failed reading source watch process list: {}",
            io::Error::last_os_error()
        ));
    }
    for pid in pids.into_iter().take(count as usize).filter(|pid| *pid > 0) {
        let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::from_mut(&mut info).cast(),
                std::mem::size_of_val(&info) as i32,
            )
        };
        if read == std::mem::size_of_val(&info) as i32
            && info.pbi_pgid == process_group
            && info.pbi_status != libc::SZOMB
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn unix_process_group_has_live_members(process_group: u32) -> Result<bool, String> {
    signal_unix_process_group(process_group, 0)
}

#[cfg(unix)]
fn signal_unix_process(pid: u32, signal: i32) -> Result<bool, String> {
    signal_unix_target(pid as i32, signal)
}

#[cfg(unix)]
fn signal_unix_process_group(process_group: u32, signal: i32) -> Result<bool, String> {
    signal_unix_target(-(process_group as i32), signal)
}

#[cfg(unix)]
fn signal_unix_process_group_for_cleanup(process_group: u32, signal: i32) -> Result<bool, String> {
    match signal_unix_process_group(process_group, signal) {
        Ok(signaled) => Ok(signaled),
        Err(signal_error) => match unix_process_group_has_live_members(process_group) {
            Ok(false) => Ok(false),
            Ok(true) => Err(signal_error),
            Err(verification_error) => Err(format!("{signal_error}; {verification_error}")),
        },
    }
}

#[cfg(unix)]
fn signal_unix_target(target: i32, signal: i32) -> Result<bool, String> {
    if unsafe { libc::kill(target, signal) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(format!(
            "failed signaling source watch process target {target}: {error}"
        ))
    }
}

#[cfg(windows)]
fn resume_windows_process(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "failed listing suspended source watch threads: {}",
            io::Error::last_os_error()
        ));
    }
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut found = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    let mut resumed = false;
    while found {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if !thread.is_null() {
                let result = unsafe { ResumeThread(thread) };
                unsafe {
                    CloseHandle(thread);
                }
                resumed = result != u32::MAX;
                break;
            }
        }
        found = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    if resumed {
        Ok(())
    } else {
        Err(format!(
            "failed resuming suspended source watch process {pid}: {}",
            io::Error::last_os_error()
        ))
    }
}

fn stop_source_watch_after_error(
    child: &mut std::process::Child,
    process_guard: &SourceWatchProcessGuard,
    primary_error: String,
) -> SourceWatchError {
    match stop_source_watch_child(child, process_guard) {
        Ok(_) => SourceWatchError::from(primary_error),
        Err(cleanup_error) => SourceWatchError::unverified(format!(
            "{}; setup also failed: {primary_error}",
            cleanup_error.message
        )),
    }
}

#[cfg(windows)]
fn stop_suspended_source_watch_after_error(
    child: &mut std::process::Child,
    guard: &SourceWatchProcessGuard,
    primary_error: String,
) -> SourceWatchError {
    if let Err(error) = child.kill() {
        return SourceWatchError::unverified(format!(
            "{SOURCE_WATCH_TREE_ACTIVE_ERROR}; failed terminating a suspended watcher: {error}; setup also failed: {primary_error}"
        ));
    }
    match child
        .wait()
        .map_err(|error| error.to_string())
        .and_then(|_| guard.stop_remaining(child.id()))
    {
        Ok(()) => SourceWatchError::from(primary_error),
        Err(error) => SourceWatchError::unverified(format!(
            "{SOURCE_WATCH_TREE_ACTIVE_ERROR}; failed reaping a suspended watcher: {error}; setup also failed: {primary_error}"
        )),
    }
}

fn source_watch_exit_code(status_code: Option<i32>, stop_requested: bool) -> i32 {
    if stop_requested {
        130
    } else {
        status_code.unwrap_or(1)
    }
}

fn source_watch_allows_service_restore<T>(watch_result: &SourceWatchResult<T>) -> bool {
    match watch_result {
        Ok(_) => true,
        Err(error) => error.cleanup_verified,
    }
}

fn source_watch_allows_override_clear(
    watch_result: &SourceWatchResult<std::process::ExitStatus>,
) -> bool {
    source_watch_allows_service_restore(watch_result)
}

fn render_dev_status(summary: &DevStatusSummary, profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        let mut lines = vec![
            format!("env={}", summary.env_name),
            format!("port={}", summary.gateway_port),
            format!("repo={}", summary.repo_root.as_deref().unwrap_or("unknown")),
            format!(
                "worktree={}",
                summary.worktree_root.as_deref().unwrap_or("unknown")
            ),
            format!("root={}", summary.root),
            format!("url={}", summary.gateway_url),
            format!("gateway_port_reachable={}", summary.gateway_port_reachable),
            format!("watch={}", summary.source_watch.state),
            format!("service_running={}", summary.service_running),
            format!(
                "service_desired_running={}",
                summary.service_desired_running
            ),
            format!("config={}", summary.config_path),
            format!("workspace={}", summary.workspace_dir),
            format!("status={}", summary.status_command),
            format!("logs={}", summary.logs_command),
        ];
        if let Some(pid) = summary.source_watch.pid {
            lines.push(format!("watch_pid={pid}"));
        }
        if let Some(issue) = &summary.source_watch.issue {
            lines.push(format!("watch_issue={issue}"));
        }
        return lines;
    }

    let mut lines = vec![paint(
        &format!("Dev env {}", summary.env_name),
        Tone::Strong,
        profile.color,
    )];
    lines.extend(render_key_value_card(
        "Status",
        &[
            KeyValueRow::accent("Port", summary.gateway_port.to_string()),
            KeyValueRow::plain("URL", summary.gateway_url.clone()),
            KeyValueRow::plain(
                "Gateway port",
                if summary.gateway_port_reachable {
                    "reachable"
                } else {
                    "unreachable"
                },
            ),
            KeyValueRow::plain("Source watch", summary.source_watch.state),
            KeyValueRow::plain("Service", dev_service_state(summary)),
        ],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Source",
        &[
            KeyValueRow::plain("Repo", summary.repo_root.as_deref().unwrap_or("unknown")),
            KeyValueRow::plain(
                "Worktree",
                summary.worktree_root.as_deref().unwrap_or("unknown"),
            ),
        ],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Next",
        &[
            KeyValueRow::plain("Status", summary.status_command.clone()),
            KeyValueRow::plain("Logs", summary.logs_command.clone()),
        ],
        profile.color,
    ));
    if let Some(issue) = &summary.source_watch.issue {
        lines.push(paint(issue, Tone::Warning, profile.color));
    }
    lines
}

fn dev_service_state(summary: &DevStatusSummary) -> &'static str {
    if summary.service_running {
        "running"
    } else if summary.service_desired_running {
        "requested, not running"
    } else if summary.service_enabled {
        "stopped"
    } else {
        "disabled"
    }
}

fn render_dev_run_summary(
    meta: &EnvMeta,
    created: bool,
    service_requested: bool,
    watch: bool,
    onboard: bool,
    profile: RenderProfile,
) -> Vec<String> {
    let Some(dev) = meta.dev.as_ref() else {
        return Vec::new();
    };
    if !profile.pretty {
        return vec![
            format!(
                "{} dev env {}",
                if created { "prepared" } else { "using" },
                meta.name
            ),
            format!("port={}", meta.gateway_port.unwrap_or_default()),
            format!("repo={}", dev.repo_root),
            format!("worktree={}", dev.worktree_root),
            format!(
                "mode={}",
                if service_requested {
                    "service"
                } else if watch {
                    "watch"
                } else {
                    "run"
                }
            ),
            format!("onboard={onboard}"),
        ];
    }

    let mut lines = vec![paint(
        &format!("Dev env {}", meta.name),
        Tone::Strong,
        profile.color,
    )];
    lines.extend(render_key_value_card(
        "Environment",
        &[
            KeyValueRow::new(
                "State",
                if created { "prepared" } else { "reusing" },
                Tone::Accent,
            ),
            KeyValueRow::accent("Port", meta.gateway_port.unwrap_or_default().to_string()),
            KeyValueRow::plain("Root", meta.root.clone()),
        ],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Source",
        &[
            KeyValueRow::plain("Repo", dev.repo_root.clone()),
            KeyValueRow::plain("Worktree", dev.worktree_root.clone()),
        ],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Launch",
        &[
            KeyValueRow::plain(
                "Mode",
                if service_requested {
                    "service"
                } else if watch {
                    "watch"
                } else {
                    "run"
                },
            ),
            KeyValueRow::plain("Onboard first", onboard.to_string()),
        ],
        profile.color,
    ));
    lines
}

fn render_dev_service_started(
    meta: &EnvMeta,
    command_example: &str,
    profile: RenderProfile,
) -> Vec<String> {
    let Some(dev) = meta.dev.as_ref() else {
        return Vec::new();
    };

    if !profile.pretty {
        return vec![
            format!("service started for {}", meta.name),
            format!("port={}", meta.gateway_port.unwrap_or_default()),
            format!(
                "url={}",
                dev_gateway_url(meta.gateway_port.unwrap_or_default())
            ),
            format!("repo={}", dev.repo_root),
            format!("worktree={}", dev.worktree_root),
            format!("status={} service status {}", command_example, meta.name),
            format!("logs={} logs {} --follow", command_example, meta.name),
        ];
    }

    let mut lines = vec![paint(
        &format!("Dev service {}", meta.name),
        Tone::Strong,
        profile.color,
    )];
    lines.extend(render_key_value_card(
        "Service",
        &[
            KeyValueRow::success("State", "running"),
            KeyValueRow::accent("Port", meta.gateway_port.unwrap_or_default().to_string()),
            KeyValueRow::plain(
                "URL",
                dev_gateway_url(meta.gateway_port.unwrap_or_default()),
            ),
            KeyValueRow::plain("Env", meta.name.clone()),
        ],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Source",
        &[
            KeyValueRow::plain("Repo", dev.repo_root.clone()),
            KeyValueRow::plain("Worktree", dev.worktree_root.clone()),
        ],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Next",
        &[
            KeyValueRow::plain(
                "Status",
                format!("{command_example} service status {}", meta.name),
            ),
            KeyValueRow::plain(
                "Logs",
                format!("{command_example} logs {} --follow", meta.name),
            ),
            KeyValueRow::plain(
                "Stop",
                format!("{command_example} service stop {}", meta.name),
            ),
        ],
        profile.color,
    ));
    lines
}

fn render_dev_service_restored(
    meta: &EnvMeta,
    command_example: &str,
    profile: RenderProfile,
) -> Vec<String> {
    let Some(dev) = meta.dev.as_ref() else {
        return Vec::new();
    };

    if !profile.pretty {
        return vec![
            format!("service restored for {}", meta.name),
            format!("port={}", meta.gateway_port.unwrap_or_default()),
            format!(
                "url={}",
                dev_gateway_url(meta.gateway_port.unwrap_or_default())
            ),
            format!("repo={}", dev.repo_root),
            format!("worktree={}", dev.worktree_root),
            format!("status={} service status {}", command_example, meta.name),
            format!("logs={} logs {} --follow", command_example, meta.name),
        ];
    }

    let mut lines = vec![paint(
        &format!("Dev service {}", meta.name),
        Tone::Strong,
        profile.color,
    )];
    lines.extend(render_key_value_card(
        "Service",
        &[
            KeyValueRow::success("State", "restored"),
            KeyValueRow::accent("Port", meta.gateway_port.unwrap_or_default().to_string()),
            KeyValueRow::plain(
                "URL",
                dev_gateway_url(meta.gateway_port.unwrap_or_default()),
            ),
        ],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Next",
        &[
            KeyValueRow::plain(
                "Status",
                format!("{command_example} service status {}", meta.name),
            ),
            KeyValueRow::plain(
                "Logs",
                format!("{command_example} logs {} --follow", meta.name),
            ),
        ],
        profile.color,
    ));
    lines
}

fn dev_gateway_url(port: u32) -> String {
    format!("http://127.0.0.1:{port}")
}

fn render_dev_run_step(title: &str, detail: String, profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        return vec![detail];
    }

    render_key_value_card(title, &[KeyValueRow::accent("Step", detail)], profile.color)
}

fn render_dev_external_plugin_warnings(
    meta: &EnvMeta,
    source_root: &Path,
    profile: RenderProfile,
) -> Vec<String> {
    let external_plugins = collect_external_installed_plugin_ids(meta, source_root);
    external_plugins
        .into_iter()
        .flat_map(|plugin_id| {
            render_dev_run_step(
                "Warning",
                format!(
                    "Installed plugin \"{plugin_id}\" is not present in {}; dev mode will keep using the env-installed plugin for that id",
                    display_path(&source_root.join("extensions"))
                ),
                profile,
            )
        })
        .collect()
}

fn render_source_watch_takeover_summary(
    meta: &EnvMeta,
    repo_root: &Path,
    profile: RenderProfile,
) -> Vec<String> {
    let log_paths = source_watch_log_paths(meta);
    if !profile.pretty {
        return vec![
            format!("taking over env {}", meta.name),
            "binding=unchanged".to_string(),
            format!("port={}", meta.gateway_port.unwrap_or_default()),
            format!("root={}", meta.root),
            format!("repo={}", display_path(repo_root)),
            format!("stdoutLog={}", display_path(&log_paths.stdout)),
            format!("stderrLog={}", display_path(&log_paths.stderr)),
            "mode=watch".to_string(),
        ];
    }

    let mut lines = vec![paint(
        &format!("Source watch {}", meta.name),
        Tone::Strong,
        profile.color,
    )];
    lines.extend(render_key_value_card(
        "Environment",
        &[
            KeyValueRow::accent("Port", meta.gateway_port.unwrap_or_default().to_string()),
            KeyValueRow::plain("Root", meta.root.clone()),
            KeyValueRow::plain("Binding", "unchanged"),
        ],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Source",
        &[KeyValueRow::plain("Repo", display_path(repo_root))],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Logs",
        &[
            KeyValueRow::plain("Stdout", display_path(&log_paths.stdout)),
            KeyValueRow::plain("Stderr", display_path(&log_paths.stderr)),
        ],
        profile.color,
    ));
    lines
}

fn collect_external_installed_plugin_ids(meta: &EnvMeta, source_root: &Path) -> BTreeSet<String> {
    let source_ids = collect_source_plugin_ids(source_root);
    collect_installed_plugin_ids(meta)
        .into_iter()
        .filter(|plugin_id| !source_ids.contains(plugin_id))
        .collect()
}

fn collect_source_plugin_ids(source_root: &Path) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let extensions_dir = source_root.join("extensions");
    let Ok(entries) = fs::read_dir(extensions_dir) else {
        return ids;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let plugin_dir = entry.path();
        if let Some(id) =
            read_json_string_at_path(&plugin_dir.join("openclaw.plugin.json"), &["id"])
        {
            ids.insert(id);
            continue;
        }
        if let Some(id) =
            read_json_string_at_path(&plugin_dir.join("package.json"), &["openclaw", "id"])
        {
            ids.insert(id);
        }
    }
    ids
}

fn collect_installed_plugin_ids(meta: &EnvMeta) -> BTreeSet<String> {
    let paths = derive_env_paths(Path::new(&meta.root));
    let mut ids = collect_installed_plugin_ids_from_json_file(&paths.config_path);
    ids.extend(collect_installed_plugin_ids_from_json_file(
        &paths.state_dir.join("plugins/installs.json"),
    ));
    ids
}

fn collect_installed_plugin_ids_from_json_file(path: &Path) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let Ok(raw) = fs::read_to_string(path) else {
        return ids;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return ids;
    };
    collect_installed_plugin_ids_from_value(&value, &mut ids);
    ids
}

fn collect_installed_plugin_ids_from_value(value: &Value, ids: &mut BTreeSet<String>) {
    if let Some(installs) = value
        .pointer("/plugins/installs")
        .and_then(Value::as_object)
    {
        ids.extend(
            installs
                .keys()
                .filter(|key| !key.trim().is_empty())
                .cloned(),
        );
    }
    if let Some(install_records) = value.get("installRecords").and_then(Value::as_object) {
        ids.extend(
            install_records
                .keys()
                .filter(|key| !key.trim().is_empty())
                .cloned(),
        );
    }
    if let Some(plugins) = value.get("plugins").and_then(Value::as_array) {
        ids.extend(
            plugins
                .iter()
                .filter_map(|plugin| plugin.get("pluginId").and_then(Value::as_str))
                .filter(|plugin_id| !plugin_id.trim().is_empty())
                .map(ToOwned::to_owned),
        );
    }
}

fn read_json_string_at_path(path: &Path, keys: &[&str]) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    let mut current = &value;
    for key in keys {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn render_source_watch_service_restored(
    meta: &EnvMeta,
    repo_root: &Path,
    command_example: &str,
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return vec![
            format!("service restored for {}", meta.name),
            format!("port={}", meta.gateway_port.unwrap_or_default()),
            format!(
                "url={}",
                dev_gateway_url(meta.gateway_port.unwrap_or_default())
            ),
            format!("repo={}", display_path(repo_root)),
            "binding=unchanged".to_string(),
            format!("status={} service status {}", command_example, meta.name),
            format!("logs={} logs {} --follow", command_example, meta.name),
        ];
    }

    let mut lines = vec![paint(
        &format!("Env service {}", meta.name),
        Tone::Strong,
        profile.color,
    )];
    lines.extend(render_key_value_card(
        "Service",
        &[
            KeyValueRow::success("State", "restored"),
            KeyValueRow::accent("Port", meta.gateway_port.unwrap_or_default().to_string()),
            KeyValueRow::plain(
                "URL",
                dev_gateway_url(meta.gateway_port.unwrap_or_default()),
            ),
            KeyValueRow::plain("Binding", "unchanged"),
        ],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Next",
        &[
            KeyValueRow::plain(
                "Status",
                format!("{command_example} service status {}", meta.name),
            ),
            KeyValueRow::plain(
                "Logs",
                format!("{command_example} logs {} --follow", meta.name),
            ),
        ],
        profile.color,
    ));
    lines
}

struct SourceWatchLogPaths {
    stdout: PathBuf,
    stderr: PathBuf,
}

struct SourceWatchLogFiles {
    stdout: File,
    stderr: File,
}

fn source_watch_log_paths(meta: &EnvMeta) -> SourceWatchLogPaths {
    let env_paths = derive_env_paths(Path::new(&meta.root));
    let logs_dir = env_paths.state_dir.join("logs");
    SourceWatchLogPaths {
        stdout: logs_dir.join("gateway.log"),
        stderr: logs_dir.join("gateway.err.log"),
    }
}

fn open_source_watch_log_files(meta: &EnvMeta) -> Result<SourceWatchLogFiles, String> {
    let paths = source_watch_log_paths(meta);
    if let Some(parent) = paths.stdout.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed creating env log directory for {}: {error}",
                meta.name
            )
        })?;
    }
    Ok(SourceWatchLogFiles {
        stdout: open_append_log(&paths.stdout, &meta.name, "stdout")?,
        stderr: open_append_log(&paths.stderr, &meta.name, "stderr")?,
    })
}

fn open_append_log(path: &Path, env_name: &str, stream: &str) -> Result<File, String> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            format!(
                "failed opening {stream} log for env \"{env_name}\": {}: {error}",
                display_path(path)
            )
        })
}

fn spawn_tee_thread<R, W>(
    input: R,
    terminal: W,
    log_file: File,
    stream: &'static str,
) -> JoinHandle<Result<(), String>>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    thread::spawn(move || {
        tee_stream(input, terminal, log_file)
            .map_err(|error| format!("failed writing source watch {stream} log: {error}"))
    })
}

fn tee_stream<R, W>(mut input: R, mut terminal: W, mut log_file: File) -> io::Result<()>
where
    R: Read,
    W: Write,
{
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let chunk = &buffer[..count];
        terminal.write_all(chunk)?;
        terminal.flush()?;
        log_file.write_all(chunk)?;
        log_file.flush()?;
    }
    Ok(())
}

fn wait_for_tee_threads(threads: Vec<JoinHandle<Result<(), String>>>) -> Result<(), String> {
    for thread in threads {
        let result = thread
            .join()
            .map_err(|_| "source watch log tee thread panicked".to_string())?;
        result?;
    }
    Ok(())
}

fn render_dev_status_list(summaries: &[DevStatusSummary], profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        let mut lines = Vec::new();
        for (index, summary) in summaries.iter().enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            lines.extend(render_dev_status(summary, profile));
        }
        return lines;
    }

    render_table(
        &[
            "Env",
            "Port",
            "Reachable",
            "Repo",
            "Worktree",
            "Watch",
            "Service",
        ],
        &summaries
            .iter()
            .map(|summary| {
                vec![
                    Cell::accent(summary.env_name.clone()),
                    Cell::right(summary.gateway_port.to_string(), Tone::Accent),
                    Cell::plain(if summary.gateway_port_reachable {
                        "yes"
                    } else {
                        "no"
                    }),
                    Cell::plain(summary.repo_root.as_deref().unwrap_or("unknown")),
                    Cell::plain(summary.worktree_root.as_deref().unwrap_or("unknown")),
                    Cell::plain(summary.source_watch.state),
                    Cell::new(
                        dev_service_state(summary),
                        crate::infra::terminal::Align::Left,
                        if summary.service_running {
                            Tone::Success
                        } else if summary.service_enabled {
                            Tone::Warning
                        } else {
                            Tone::Muted
                        },
                    ),
                ]
            })
            .collect::<Vec<_>>(),
        profile.color,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        DevSourceWatchSummary, DevStatusSummary, RenderProfile, SourceWatchError,
        combine_watch_and_restore_results, render_dev_status, source_watch_allows_service_restore,
        source_watch_exit_code, source_watch_stop_timeout_error,
    };
    use crate::service::ServiceActionSummary;

    #[cfg(unix)]
    #[test]
    fn source_startup_gate_precedes_native_node_preloads() {
        use std::io::Write as _;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::process::CommandExt as _;
        struct OwnedChild(std::process::Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        for (option, release) in [
            ("--require", true),
            ("--import", true),
            ("--require", false),
        ] {
            let root = tempfile::TempDir::new().unwrap();
            std::fs::create_dir(root.path().join("scripts")).unwrap();
            let preload = root.path().join(if option == "--require" {
                "preload file.cjs"
            } else {
                "preload file.mjs"
            });
            let preload_marker = root.path().join("preloaded");
            let entry_marker = root.path().join("source-ran");
            let stderr_path = root.path().join("stderr.log");
            std::fs::write(
                &preload,
                if option == "--require" {
                    "require('node:fs').writeFileSync('preloaded', 'loaded');\n"
                } else {
                    "import fs from 'node:fs'; fs.writeFileSync('preloaded', 'loaded');\n"
                },
            )
            .unwrap();
            std::fs::write(
                root.path().join("scripts/watch-node.mjs"),
                r#"
import fs from 'node:fs';
fs.writeFileSync('source-ran', JSON.stringify({
  args: process.argv.slice(2), stdin: fs.readFileSync(0, 'utf8'),
  pendingGate: process.env.OCM_SOURCE_WATCH_START_FD,
  consumedGate: process.env.OCM_SOURCE_WATCH_RELEASED_FD,
}));
"#,
            )
            .unwrap();
            let args = vec!["gateway".to_string(), "value with spaces".to_string()];
            let mut command = super::source_watch_node_command(&args);
            command
                .current_dir(root.path())
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", root.path())
                .env(
                    "NODE_OPTIONS",
                    format!(
                        "{option} {}",
                        serde_json::to_string(&preload.to_string_lossy()).unwrap()
                    ),
                )
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::from(
                    std::fs::File::create(&stderr_path).unwrap(),
                ))
                .process_group(0);
            let mut guard = super::SourceWatchProcessGuard::new_with_terminal(false).unwrap();
            // Other tests may free low descriptors concurrently; reserve the
            // high descriptor explicitly instead of relying on allocation order.
            let fd =
                unsafe { libc::fcntl(guard.startup_reader.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
            assert!(fd >= 10, "{}", std::io::Error::last_os_error());
            guard.startup_reader = unsafe { std::io::PipeReader::from_raw_fd(fd) };
            guard.configure_command(&mut command).unwrap();
            let mut child = OwnedChild(command.spawn().unwrap());
            child
                .0
                .stdin
                .take()
                .unwrap()
                .write_all(b"original stdin")
                .unwrap();
            let before_release = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < before_release && !preload_marker.exists() {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "{}",
                    std::fs::read_to_string(&stderr_path).unwrap()
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if preload_marker.exists() {
                child.0.kill().unwrap();
                child.0.wait().unwrap();
                panic!("Node preload executed before startup release; owned child reaped");
            }
            assert!(!entry_marker.exists());
            if release {
                guard.start_child(&child.0).unwrap();
            }
            drop(guard);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let status = loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "gated child did not finish"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            assert_eq!(
                status.success(),
                release,
                "{}",
                std::fs::read_to_string(&stderr_path).unwrap()
            );
            assert_eq!(
                preload_marker.exists(),
                release,
                "preload release/EOF behavior"
            );
            assert_eq!(
                entry_marker.exists(),
                release,
                "source release/EOF behavior"
            );
            if release {
                let entry: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&entry_marker).unwrap()).unwrap();
                assert_eq!(entry["args"], serde_json::json!(args));
                assert_eq!(entry["stdin"], "original stdin");
                assert!(entry["pendingGate"].is_null() && entry["consumedGate"].is_null());
            }
        }
    }

    fn sample_summary() -> DevStatusSummary {
        DevStatusSummary {
            env_name: "demo".to_string(),
            root: "/tmp/demo".to_string(),
            repo_root: Some("/repo/openclaw".to_string()),
            worktree_root: Some("/repo/openclaw/.worktrees/demo".to_string()),
            gateway_port: 18789,
            gateway_url: "http://127.0.0.1:18789".to_string(),
            gateway_port_reachable: true,
            config_path: "/tmp/demo/.openclaw/openclaw.json".to_string(),
            workspace_dir: "/tmp/demo/.openclaw/workspace".to_string(),
            service_enabled: true,
            service_running: true,
            service_desired_running: true,
            service_pid: Some(123),
            source_watch: DevSourceWatchSummary {
                state: "inactive",
                pid: None,
                started_at: None,
                issue: None,
            },
            logs_command: "ocm logs demo --follow".to_string(),
            status_command: "ocm dev status demo".to_string(),
        }
    }

    #[test]
    fn dev_status_pretty_stays_compact_when_healthy() {
        let lines = render_dev_status(&sample_summary(), RenderProfile::pretty(false));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("http://127.0.0.1:18789"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm logs demo --follow"))
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("/tmp/demo/.openclaw/openclaw.json"))
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("/tmp/demo/.openclaw/workspace"))
        );
        assert!(!lines.iter().any(|line| line.contains("Service enabled")));
    }

    #[test]
    fn source_watch_stop_timeout_preserves_service_diagnostics() {
        let summary = ServiceActionSummary {
            env_name: "demo".to_string(),
            service_kind: "supervisor".to_string(),
            action: "stop".to_string(),
            installed: true,
            loaded: true,
            running: true,
            desired_running: false,
            gateway_port: 18789,
            gateway_state: "stopping".to_string(),
            gateway_ready: None,
            issue: None,
            binding_kind: Some("runtime".to_string()),
            binding_name: Some("stable".to_string()),
            stdout_path: None,
            stderr_path: None,
            warnings: vec!["gateway is still shutting down".to_string()],
        };

        assert_eq!(
            source_watch_stop_timeout_error(&summary),
            "background service for demo is still running after the stop request (gateway is still shutting down)"
        );
    }

    #[test]
    fn source_watch_reports_watch_and_restore_failures_together() {
        let result = combine_watch_and_restore_results(
            Err(SourceWatchError::from("watch failed".to_string())),
            Err("restore failed".to_string()),
            "demo",
        );

        assert_eq!(
            result,
            Err(
                "watch failed; also failed restoring background service for demo: restore failed"
                    .to_string()
            )
        );
    }

    #[test]
    fn source_watch_preserves_the_original_interrupt_exit_code() {
        assert_eq!(source_watch_exit_code(Some(143), true), 130);
        assert_eq!(source_watch_exit_code(Some(23), false), 23);
        assert_eq!(source_watch_exit_code(None, false), 1);
    }

    #[test]
    fn source_watch_does_not_restore_over_a_live_process_tree() {
        assert!(source_watch_allows_service_restore(&Ok::<
            _,
            SourceWatchError,
        >(0)));
        assert!(source_watch_allows_service_restore(&Err::<i32, _>(
            SourceWatchError::from("source watch failed".to_string())
        )));
        assert!(!source_watch_allows_service_restore(&Err::<i32, _>(
            SourceWatchError::unverified("source watch process tree is still active: 123")
        )));
    }
}
