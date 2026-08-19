use std::collections::BTreeSet;
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
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
#[cfg(windows)]
use std::os::windows::{io::AsRawHandle, process::CommandExt as _};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Cli;
use super::render::RenderProfile;
use crate::env::{
    CreateEnvironmentOptions, CreateSourceWatchOverrideOptions, EnvDevMeta, EnvMeta,
    SourceWatchLease,
};
use crate::infra::process::run_direct;
use crate::infra::shell::{build_openclaw_dev_source_env, build_openclaw_env};
use crate::infra::terminal::{Cell, KeyValueRow, Tone, paint, render_key_value_card, render_table};
use crate::openclaw_repo::{
    detect_openclaw_checkout, discover_openclaw_checkout, ensure_checkout_owned_dependencies,
    ensure_openclaw_worktree,
};
use crate::service::service_backend_support_error;
use crate::store::{
    derive_env_paths, display_path, ensure_minimum_local_openclaw_config, ensure_store, read_json,
    resolve_absolute_path, validate_name, write_json,
};

const DEV_PREFERENCES_KIND: &str = "ocm-dev-preferences";
const SOURCE_WATCH_TREE_ACTIVE_ERROR: &str = "source watch process tree is still active";
#[cfg(unix)]
const SOURCE_WATCH_NODE_SHIM: &str = r#"import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
const startFd = Number(process.env.OCM_SOURCE_WATCH_START_FD);
delete process.env.OCM_SOURCE_WATCH_START_FD;
if (Number.isInteger(startFd) && fs.readSync(startFd, Buffer.alloc(1), 0, 1, null) !== 1) {
  process.exit(1);
}
const script = path.resolve("scripts/watch-node.mjs");
process.argv = [process.execPath, script, ...process.argv.slice(1)];
if (process.env.OCM_SOURCE_WATCH_FORCE_TTY === "1") {
  delete process.env.OCM_SOURCE_WATCH_FORCE_TTY;
  Object.defineProperty(process.stdin, "isTTY", { value: true });
}
await import(pathToFileURL(script).href);"#;

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevPreferences {
    kind: String,
    preferred_repo_root: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DevStatusSummary {
    env_name: String,
    root: String,
    repo_root: String,
    worktree_root: String,
    gateway_port: u32,
    gateway_url: String,
    config_path: String,
    workspace_dir: String,
    service_enabled: bool,
    service_running: bool,
    logs_command: String,
    status_command: String,
}

struct ExistingEnvSourceWatchOptions {
    repo_root: Option<String>,
    root: Option<String>,
    gateway_port: Option<u32>,
    watch: bool,
    force: bool,
    onboard: bool,
}

impl Cli {
    pub(super) fn handle_dev_command(&self, args: Vec<String>) -> Result<i32, String> {
        match args.first().map(String::as_str).unwrap_or("") {
            "" | "help" | "--help" | "-h" => self.dispatch_help_command(vec!["dev".to_string()]),
            "status" => self.handle_dev_status(args[1..].to_vec()),
            _ => self.handle_dev_run(args),
        }
    }

    fn handle_dev_status(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "dev status")?;
        let target = args.first().cloned();
        Self::assert_no_extra_args(&args[target.is_some() as usize..])?;

        let envs = self.environment_service().list()?;
        let mut summaries = envs
            .into_iter()
            .filter_map(|meta| self.build_dev_status_summary(meta).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        summaries.sort_by(|left, right| left.env_name.cmp(&right.env_name));

        if let Some(target) = target {
            let target = validate_name(&target, "Environment name")?;
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

        if let Some(existing) = self.environment_service().find(&name)?
            && existing.dev.is_none()
        {
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

        let (meta, created) = self.ensure_dev_env(&name, repo_root, root, gateway_port)?;
        let dev = meta
            .dev
            .as_ref()
            .ok_or_else(|| format!("environment \"{}\" is missing its dev binding", meta.name))?;
        let stderr_profile = self.dev_stderr_profile();
        let watch_takes_over_service = watch && force && meta.service_running;
        if !service_requested && meta.service_running && !watch_takes_over_service {
            return Err(format!(
                "dev env {} is already running in the background; stop it first with {} service stop {}, inspect it with {} logs {} --follow, or rerun with --watch --force to take it over temporarily",
                meta.name,
                self.command_example(),
                meta.name,
                self.command_example(),
                meta.name
            ));
        }
        let mut source_watch_lease = if watch {
            Some(
                self.environment_service()
                    .acquire_source_watch_lease(&meta.name)?,
            )
        } else {
            None
        };
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

        let install_code = self.ensure_dev_dependencies(&meta)?;
        if install_code != 0 {
            return Ok(install_code);
        }

        if onboard {
            self.stderr_lines(render_dev_run_step(
                "Onboarding",
                format!("Running local onboarding in {}", dev.worktree_root),
                stderr_profile,
            ));
            let code = self.run_dev_onboard(&meta)?;
            if code != 0 {
                return Ok(code);
            }
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
                if let Err(stop_error) = self.stop_service_for_source_watch(&meta.name) {
                    let lease = source_watch_lease
                        .as_mut()
                        .ok_or_else(|| "source watch lease is missing".to_string())?;
                    return Err(self.restore_service_policy_after_failed_takeover(
                        &meta.name, stop_error, lease,
                    ));
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
                    .as_ref()
                    .ok_or_else(|| "source watch lease is missing".to_string())?,
            );
            let restore_result = if watch_takes_over_service
                && source_watch_allows_service_restore(&watch_result)
            {
                let restore_state_result = source_watch_lease
                    .as_mut()
                    .ok_or_else(|| "source watch lease is missing".to_string())?
                    .begin_service_restore();
                match restore_state_result {
                    Ok(()) => {
                        self.stderr_lines(render_dev_run_step(
                            "Restore",
                            format!("Starting background service for {}", meta.name),
                            stderr_profile,
                        ));
                        self.service_service().start(&meta.name).map(|_| {
                            self.stdout_lines(render_dev_service_restored(
                                &meta,
                                &self.command_example(),
                                self.dev_stdout_profile(),
                            ));
                        })
                    }
                    Err(error) => Err(format!(
                        "failed preparing background service restoration: {error}; the service remains stopped to preserve source-watch exclusivity"
                    )),
                }
            } else {
                Ok(())
            };
            drop(source_watch_lease.take());
            return combine_watch_and_restore_results(watch_result, restore_result, &meta.name);
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
        ensure_checkout_owned_dependencies(&repo_root)?;
        let meta = self
            .environment_service()
            .apply_effective_gateway_port(existing)?;
        let mut source_watch_lease = Some(
            self.environment_service()
                .acquire_source_watch_lease(&meta.name)?,
        );
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

        let restore_service = meta.service_running;
        if restore_service {
            self.stderr_lines(render_dev_run_step(
                "Takeover",
                format!(
                    "Stopping background service for {} while source watch takes over; OCM will restore it when watch exits",
                    meta.name
                ),
                stderr_profile,
            ));
            if let Err(stop_error) = self.stop_service_for_source_watch(&meta.name) {
                let lease = source_watch_lease
                    .as_mut()
                    .ok_or_else(|| "source watch lease is missing".to_string())?;
                return Err(self
                    .restore_service_policy_after_failed_takeover(&meta.name, stop_error, lease));
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
                .as_ref()
                .ok_or_else(|| "source watch lease is missing".to_string())?,
        );

        let restore_result = if restore_service
            && source_watch_allows_service_restore(&watch_result)
        {
            let restore_state_result = source_watch_lease
                .as_mut()
                .ok_or_else(|| "source watch lease is missing".to_string())?
                .begin_service_restore();
            match restore_state_result {
                Ok(()) => {
                    self.stderr_lines(render_dev_run_step(
                        "Restore",
                        format!("Starting background service for {}", meta.name),
                        stderr_profile,
                    ));
                    self.service_service().start(&meta.name).map(|_| {
                        self.stdout_lines(render_source_watch_service_restored(
                            &meta,
                            &repo_root,
                            &self.command_example(),
                            self.dev_stdout_profile(),
                        ));
                    })
                }
                Err(error) => Err(format!(
                    "failed preparing background service restoration: {error}; the service remains stopped to preserve source-watch exclusivity"
                )),
            }
        } else {
            Ok(())
        };
        drop(source_watch_lease.take());

        combine_watch_and_restore_results(watch_result, restore_result, &meta.name)
    }

    fn stop_service_for_source_watch(&self, env_name: &str) -> Result<(), String> {
        let stop_result = self.service_service().stop(env_name);
        match stop_result {
            Ok(summary) if !summary.running => Ok(()),
            Ok(summary) => Err(source_watch_stop_timeout_error(&summary)),
            Err(error) => Err(format!(
                "failed stopping background service for {env_name}: {error}"
            )),
        }
    }

    fn restore_service_policy_after_failed_takeover(
        &self,
        env_name: &str,
        stop_error: String,
        source_watch_lease: &mut SourceWatchLease,
    ) -> String {
        if let Err(restore_state_error) = source_watch_lease.begin_service_restore() {
            return format!(
                "{stop_error}; also failed preparing background service restoration: {restore_state_error}"
            );
        }
        match self.service_service().start(env_name) {
            Ok(_) => format!(
                "{stop_error}; restored the background service policy and did not start source watch"
            ),
            Err(restore_error) => format!(
                "{stop_error}; also failed restoring the background service policy: {restore_error}"
            ),
        }
    }

    fn ensure_dev_env(
        &self,
        name: &str,
        repo_root: Option<String>,
        root: Option<String>,
        gateway_port: Option<u32>,
    ) -> Result<(EnvMeta, bool), String> {
        if let Some(existing) = self.environment_service().find(name)? {
            let dev = existing.dev.as_ref().ok_or_else(|| {
                format!(
                    "environment \"{}\" is not a dev env; use a new env name for `ocm dev`",
                    existing.name
                )
            })?;
            let existing_repo = PathBuf::from(&dev.repo_root);
            if let Some(repo_root) = repo_root {
                let requested = resolve_absolute_path(&repo_root, &self.env, &self.cwd)?;
                if requested != existing_repo {
                    return Err(format!(
                        "dev cannot change the repo for existing env {}; current repo is {}",
                        existing.name, dev.repo_root
                    ));
                }
            }
            if let Some(root) = root {
                let requested = resolve_absolute_path(&root, &self.env, &self.cwd)?;
                let current = PathBuf::from(&existing.root);
                if requested != current {
                    return Err(format!(
                        "dev cannot change the root for existing env {}; current root is {}",
                        existing.name, existing.root
                    ));
                }
            }

            ensure_openclaw_worktree(&existing_repo, &existing.name)?;
            let meta = self
                .environment_service()
                .apply_effective_gateway_port(existing)?;
            if let Some(requested_port) = gateway_port {
                let current_port = meta.gateway_port.unwrap_or_default();
                if requested_port != current_port {
                    return Err(format!(
                        "dev cannot change the port for existing env {}; current port is {}",
                        meta.name, current_port
                    ));
                }
            }
            self.save_preferred_dev_repo(&existing_repo)?;
            self.bootstrap_dev_env(&meta)?;
            return Ok((meta, false));
        }

        let repo_root = self.resolve_dev_repo_root(repo_root)?;
        let repo_root = detect_openclaw_checkout(&repo_root).ok_or_else(|| {
            format!(
                "OpenClaw checkout not found at {}",
                display_path(&repo_root)
            )
        })?;
        self.save_preferred_dev_repo(&repo_root)?;
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

        let created = self
            .environment_service()
            .apply_effective_gateway_port(created)?;
        if let Err(error) = self.bootstrap_dev_env(&created) {
            let _ = self.environment_service().remove(&created.name, true);
            return Err(error);
        }

        Ok((created, true))
    }

    fn resolve_dev_repo_root(&self, repo_root: Option<String>) -> Result<PathBuf, String> {
        if let Some(repo_root) = repo_root {
            return resolve_absolute_path(&repo_root, &self.env, &self.cwd);
        }

        if let Some(repo_root) = discover_openclaw_checkout(&self.cwd) {
            return Ok(repo_root);
        }

        if let Some(repo_root) = self.load_preferred_dev_repo()?
            && let Some(repo_root) = detect_openclaw_checkout(&repo_root)
        {
            return Ok(repo_root);
        }

        let repo_root = self.prompt_dev_repo_root()?;
        resolve_absolute_path(&repo_root, &self.env, &self.cwd)
    }

    fn prompt_dev_repo_root(&self) -> Result<String, String> {
        loop {
            let value = self.prompt_required("OpenClaw repo path").map_err(|_| {
                "OpenClaw repo path is required; pass --repo /path/to/openclaw".to_string()
            })?;
            let repo_root = resolve_absolute_path(&value, &self.env, &self.cwd)?;
            if detect_openclaw_checkout(&repo_root).is_some() {
                return Ok(display_path(&repo_root));
            }
            self.stderr_line(format!(
                "ocm: OpenClaw checkout not found at {}",
                display_path(&repo_root)
            ));
        }
    }

    fn load_preferred_dev_repo(&self) -> Result<Option<PathBuf>, String> {
        let path = self.dev_preferences_path()?;
        if !path.exists() {
            return Ok(None);
        }

        let prefs = read_json::<DevPreferences>(&path)?;
        Ok(prefs.preferred_repo_root.map(PathBuf::from))
    }

    fn save_preferred_dev_repo(&self, repo_root: &Path) -> Result<(), String> {
        let path = self.dev_preferences_path()?;
        let prefs = DevPreferences {
            kind: DEV_PREFERENCES_KIND.to_string(),
            preferred_repo_root: Some(display_path(repo_root)),
        };
        write_json(&path, &prefs)
    }

    fn dev_preferences_path(&self) -> Result<PathBuf, String> {
        let stores = ensure_store(&self.env, &self.cwd)?;
        Ok(stores.home.join("dev.json"))
    }

    fn bootstrap_dev_env(&self, meta: &EnvMeta) -> Result<(), String> {
        let paths = derive_env_paths(Path::new(&meta.root));
        let (gateway_port, _) = self
            .environment_service()
            .resolve_effective_gateway_port(meta)?;
        ensure_minimum_local_openclaw_config(&paths, gateway_port)
    }

    fn ensure_dev_dependencies(&self, meta: &EnvMeta) -> Result<i32, String> {
        let dev = meta
            .dev
            .as_ref()
            .ok_or_else(|| format!("environment \"{}\" is missing its dev binding", meta.name))?;
        let worktree_root = Path::new(&dev.worktree_root);
        ensure_checkout_owned_dependencies(worktree_root)?;
        let pnpm_store = worktree_root.join("node_modules").join(".pnpm");
        let tsx_bin = worktree_root.join("node_modules").join(".bin").join("tsx");
        if pnpm_store.exists() && tsx_bin.exists() {
            return Ok(0);
        }

        self.stderr_lines(render_dev_run_step(
            "Dependencies",
            format!("Installing dependencies in {}", dev.worktree_root),
            self.dev_stderr_profile(),
        ));
        run_direct(
            "pnpm",
            &["install".to_string()],
            &build_openclaw_env(meta, &self.env),
            worktree_root,
        )
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

    fn run_dev_onboard(&self, meta: &EnvMeta) -> Result<i32, String> {
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
        run_direct(
            "pnpm",
            &args,
            &build_openclaw_dev_source_env(meta, &self.env, Path::new(&dev.worktree_root)),
            Path::new(&dev.worktree_root),
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
        source_watch_lease: &SourceWatchLease,
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
        )
    }

    fn run_source_gateway_watch(
        &self,
        meta: &EnvMeta,
        repo_root: &Path,
        tee_to_env_logs: bool,
        _source_watch_lease: &SourceWatchLease,
    ) -> SourceWatchResult<i32> {
        let args = [
            "scripts/watch-node.mjs".to_string(),
            "gateway".to_string(),
            "run".to_string(),
            "--port".to_string(),
            meta.gateway_port.unwrap_or_default().to_string(),
        ];
        let stop_requested = Arc::new(AtomicBool::new(false));
        let signal_flag = Arc::clone(&stop_requested);
        ctrlc::set_handler(move || {
            signal_flag.store(true, Ordering::SeqCst);
        })
        .map_err(|error| format!("failed to install dev watch signal handler: {error}"))?;

        let mut command = Command::new("node");
        #[cfg(unix)]
        {
            command.args(["--input-type=module", "--eval", SOURCE_WATCH_NODE_SHIM]);
            command.args(&args[1..]);
        }
        #[cfg(unix)]
        let source_watch_force_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } != 1;
        #[cfg(windows)]
        // watch-node never detaches runners on win32; the Job Object owns the full tree.
        command.args(&args);
        #[cfg(not(any(unix, windows)))]
        command.args(&args);
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
        _source_watch_lease.configure_child(&mut command);

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
        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to run \"node\": {error}"))?;
        if let Err(error) = process_guard.assign_child(&child) {
            #[cfg(windows)]
            return Err(stop_suspended_source_watch_after_error(&mut child, error));
            #[cfg(not(windows))]
            return Err(stop_source_watch_after_error(
                &mut child,
                &process_guard,
                error,
            ));
        }
        if let Err(error) = _source_watch_lease.attach_to_child(&child) {
            #[cfg(windows)]
            return Err(stop_suspended_source_watch_after_error(&mut child, error));
            #[cfg(not(windows))]
            return Err(stop_source_watch_after_error(
                &mut child,
                &process_guard,
                error,
            ));
        }
        if let Err(error) = process_guard.start_child(&child) {
            #[cfg(windows)]
            return Err(stop_suspended_source_watch_after_error(&mut child, error));
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
                    watch_pid: child.id(),
                },
                _source_watch_lease,
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
            match wait_for_source_watch_child(&mut child, &stop_requested, &process_guard) {
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

        let status = combine_source_watch_cleanup_results(status_result, tee_result, clear_result)?;
        let mut status_code = status.code();
        #[cfg(unix)]
        if status_code.is_none() {
            status_code = status.signal().map(|signal| 128 + signal);
        }
        Ok(source_watch_exit_code(
            status_code,
            stop_requested.load(Ordering::SeqCst),
        ))
    }

    fn build_dev_status_summary(&self, meta: EnvMeta) -> Result<Option<DevStatusSummary>, String> {
        let Some(dev) = meta.dev.as_ref() else {
            return Ok(None);
        };
        let (gateway_port, _) = self
            .environment_service()
            .resolve_effective_gateway_port(&meta)?;
        let paths = derive_env_paths(Path::new(&meta.root));
        let env_name = meta.name.clone();
        Ok(Some(DevStatusSummary {
            env_name: env_name.clone(),
            root: meta.root,
            repo_root: dev.repo_root.clone(),
            worktree_root: dev.worktree_root.clone(),
            gateway_port,
            gateway_url: dev_gateway_url(gateway_port),
            config_path: display_path(&paths.config_path),
            workspace_dir: display_path(&paths.workspace_dir),
            service_enabled: meta.service_enabled,
            service_running: meta.service_running,
            logs_command: format!("{} logs {} --follow", self.command_example(), env_name),
            status_command: format!("{} service status {}", self.command_example(), env_name),
        }))
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
) -> SourceWatchResult<std::process::ExitStatus> {
    loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            SourceWatchError::unverified(format!("failed waiting for source watch: {error}"))
        })? {
            process_guard
                .stop_remaining(child.id())
                .map_err(SourceWatchError::unverified)?;
            return Ok(status);
        }
        if stop_requested.load(Ordering::SeqCst) {
            return stop_source_watch_child(child, process_guard);
        }
        #[cfg(unix)]
        {
            let mut status = 0;
            let waited = unsafe {
                libc::waitpid(
                    child.id() as libc::pid_t,
                    &mut status,
                    libc::WNOHANG | libc::WUNTRACED,
                )
            };
            if waited == -1 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(SourceWatchError::unverified(format!(
                        "failed checking source watch job-control state: {error}"
                    )));
                }
            } else if waited != 0 {
                if libc::WIFSTOPPED(status) {
                    process_guard
                        .suspend_with_child(child.id())
                        .map_err(SourceWatchError::unverified)?;
                } else if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                    process_guard
                        .stop_remaining(child.id())
                        .map_err(SourceWatchError::unverified)?;
                    return Ok(std::process::ExitStatus::from_raw(status));
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

struct SourceWatchProcessGuard {
    #[cfg(unix)]
    terminal: Option<SourceWatchTerminalGuard>,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(unix)]
struct SourceWatchTerminalGuard {
    parent_process_group: libc::pid_t,
    startup_reader: Option<UnixStream>,
    startup_writer: Option<UnixStream>,
    foreground_assigned: AtomicBool,
}

impl SourceWatchProcessGuard {
    fn new() -> Result<Self, String> {
        #[cfg(unix)]
        {
            let terminal = if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
                let foreground_process_group = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
                if foreground_process_group == -1 {
                    return Err(format!(
                        "failed reading source watch terminal ownership: {}",
                        io::Error::last_os_error()
                    ));
                }
                let parent_process_group = unsafe { libc::getpgrp() };
                let (startup_reader, startup_writer) =
                    if foreground_process_group == parent_process_group {
                        let (reader, writer) = UnixStream::pair().map_err(|error| {
                            format!("failed creating source watch startup gate: {error}")
                        })?;
                        (Some(reader), Some(writer))
                    } else {
                        (None, None)
                    };
                Some(SourceWatchTerminalGuard {
                    parent_process_group,
                    startup_reader,
                    startup_writer,
                    foreground_assigned: AtomicBool::new(false),
                })
            } else {
                None
            };
            Ok(Self { terminal })
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
        if let Some(startup_reader) = self
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.startup_reader.as_ref())
        {
            let startup_fd = startup_reader.as_raw_fd();
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
            && terminal.startup_reader.is_some()
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
        if let Some(startup_writer) = self
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.startup_writer.as_ref())
        {
            let mut writer = startup_writer;
            writer
                .write_all(&[1])
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
    fn suspend_with_child(&self, child_pid: u32) -> Result<(), String> {
        self.restore_terminal()?;
        loop {
            if unsafe { libc::kill(libc::getpid(), libc::SIGSTOP) } == -1 {
                return Err(format!(
                    "failed suspending OCM with source watch: {}",
                    io::Error::last_os_error()
                ));
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
            let deadline = std::time::Instant::now() + Duration::from_secs(7);
            while std::time::Instant::now() < deadline {
                if let Some(status) = child.try_wait().map_err(|error| {
                    SourceWatchError::unverified(format!(
                        "failed waiting for source watch shutdown: {error}"
                    ))
                })? {
                    process_guard
                        .stop_remaining(child.id())
                        .map_err(SourceWatchError::unverified)?;
                    return Ok(status);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
        signal_unix_process_group_for_cleanup(child.id(), libc::SIGSTOP)
            .map_err(SourceWatchError::unverified)?;
        signal_unix_process_group_for_cleanup(child.id(), libc::SIGKILL)
            .map_err(SourceWatchError::unverified)?;
        let status = child.wait().map_err(|error| {
            SourceWatchError::unverified(format!(
                "failed waiting for stopped source watch: {error}"
            ))
        })?;
        wait_for_unix_process_group_to_stop(child.id()).map_err(SourceWatchError::unverified)?;
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
    primary_error: String,
) -> SourceWatchError {
    if let Err(error) = child.kill() {
        return SourceWatchError::unverified(format!(
            "{SOURCE_WATCH_TREE_ACTIVE_ERROR}; failed terminating a suspended watcher: {error}; setup also failed: {primary_error}"
        ));
    }
    match child.wait() {
        Ok(_) => SourceWatchError::from(primary_error),
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
        return vec![
            format!("env={}", summary.env_name),
            format!("port={}", summary.gateway_port),
            format!("repo={}", summary.repo_root),
            format!("worktree={}", summary.worktree_root),
            format!("root={}", summary.root),
            format!("url={}", summary.gateway_url),
            format!("config={}", summary.config_path),
            format!("workspace={}", summary.workspace_dir),
            format!("status={}", summary.status_command),
            format!("logs={}", summary.logs_command),
        ];
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
                "Service",
                if summary.service_running {
                    "running".to_string()
                } else if summary.service_enabled {
                    "enabled".to_string()
                } else {
                    "disabled".to_string()
                },
            ),
        ],
        profile.color,
    ));
    lines.extend(render_key_value_card(
        "Source",
        &[
            KeyValueRow::plain("Repo", summary.repo_root.clone()),
            KeyValueRow::plain("Worktree", summary.worktree_root.clone()),
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
    lines
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
        &["Env", "Port", "Repo", "Worktree", "Service"],
        &summaries
            .iter()
            .map(|summary| {
                vec![
                    Cell::accent(summary.env_name.clone()),
                    Cell::right(summary.gateway_port.to_string(), Tone::Accent),
                    Cell::plain(summary.repo_root.clone()),
                    Cell::plain(summary.worktree_root.clone()),
                    Cell::new(
                        if summary.service_running {
                            "running"
                        } else if summary.service_enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
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
        DevStatusSummary, RenderProfile, SourceWatchError, combine_watch_and_restore_results,
        render_dev_status, source_watch_allows_service_restore, source_watch_exit_code,
        source_watch_stop_timeout_error,
    };
    use crate::service::ServiceActionSummary;

    fn sample_summary() -> DevStatusSummary {
        DevStatusSummary {
            env_name: "demo".to_string(),
            root: "/tmp/demo".to_string(),
            repo_root: "/repo/openclaw".to_string(),
            worktree_root: "/repo/openclaw/.worktrees/demo".to_string(),
            gateway_port: 18789,
            gateway_url: "http://127.0.0.1:18789".to_string(),
            config_path: "/tmp/demo/.openclaw/openclaw.json".to_string(),
            workspace_dir: "/tmp/demo/.openclaw/workspace".to_string(),
            service_enabled: true,
            service_running: true,
            logs_command: "ocm logs demo --follow".to_string(),
            status_command: "ocm service status demo".to_string(),
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
