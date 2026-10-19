use std::path::Path;

use serde::Serialize;

use super::Cli;
use crate::env::{CreateEnvironmentOptions, EnvMeta};
use crate::infra::terminal::{KeyValueRow, Tone, paint, render_key_value_card};
use crate::launcher::AddLauncherOptions;
use crate::migrate::inspect_migration_source;
use crate::service::service_backend_support_error;
use crate::store::{
    derive_env_paths, display_path, ensure_minimum_local_openclaw_config, resolve_absolute_path,
    validate_name,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartSummary {
    env_name: String,
    created: bool,
    root: String,
    gateway_port: u32,
    gateway_port_source: String,
    config_mode: String,
    default_runtime: Option<String>,
    default_launcher: Option<String>,
    protected: bool,
    onboarding_planned: bool,
    service_requested: bool,
    service_started: bool,
    detected_plain_home: Option<String>,
    migrate_command: Option<String>,
    open_command: String,
    run_command: String,
    onboard_command: String,
    service_command: String,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum StartOnboardingMode {
    Always,
    Never,
}

#[derive(Clone, Debug)]
pub(super) struct StartRequest {
    pub name: String,
    pub root: Option<String>,
    pub gateway_port: Option<u32>,
    pub protect: bool,
    pub service_requested: bool,
    pub onboarding_mode: StartOnboardingMode,
    pub runtime_name: Option<String>,
    pub launcher_name: Option<String>,
    pub version: Option<String>,
    pub channel: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug)]
enum StartBinding {
    Runtime(String),
    Launcher(String),
}

impl Cli {
    pub(super) fn handle_start_command(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag) = Self::consume_flag(args, "--json");
        let (args, protect) = Self::consume_flag(args, "--protect");
        let (args, onboard) = Self::consume_flag(args, "--onboard");
        let (args, no_service) = Self::consume_flag(args, "--no-service");
        let (args, root) = Self::consume_option(args, "--root")?;
        let root = Self::require_option_value(root, "--root")?;
        let (args, port_raw) = Self::consume_option(args, "--port")?;
        let gateway_port = match port_raw.as_deref() {
            Some(raw) => Some(Self::parse_gateway_port(raw)?),
            None => None,
        };
        let (args, runtime_name) = Self::consume_option(args, "--runtime")?;
        let runtime_name = Self::require_option_value(runtime_name, "--runtime")?;
        let (args, launcher_name) = Self::consume_option(args, "--launcher")?;
        let launcher_name = Self::require_option_value(launcher_name, "--launcher")?;
        let (args, version) = Self::consume_option(args, "--version")?;
        let version = Self::require_option_value(version, "--version")?;
        let (args, channel) = Self::consume_option(args, "--channel")?;
        let channel = Self::require_option_value(channel, "--channel")?;
        let (args, command) = Self::consume_option(args, "--command")?;
        let command = Self::require_option_value(command, "--command")?;
        let (args, cwd) = Self::consume_option(args, "--cwd")?;
        let cwd = Self::require_option_value(cwd, "--cwd")?;

        if cwd.is_some() && command.is_none() {
            return Err("start accepts --cwd only with --command".to_string());
        }
        if version.is_some() && channel.is_some() {
            return Err("start accepts only one of --version or --channel".to_string());
        }

        let binding_sources = runtime_name.is_some() as u8
            + launcher_name.is_some() as u8
            + command.is_some() as u8
            + (version.is_some() || channel.is_some()) as u8;
        if binding_sources > 1 {
            return Err(
                "start accepts only one binding source: --runtime, --launcher, --version/--channel, or --command"
                    .to_string(),
            );
        }

        let name = match args.as_slice() {
            [] => self.suggest_generated_env_name(),
            [name] => validate_name(name, "Environment name")?,
            [name, extra @ ..] => {
                Self::assert_no_extra_args(extra)?;
                validate_name(name, "Environment name")?
            }
        };

        let request = StartRequest {
            name,
            root,
            gateway_port,
            protect,
            service_requested: !no_service,
            onboarding_mode: if onboard {
                StartOnboardingMode::Always
            } else {
                StartOnboardingMode::Never
            },
            runtime_name,
            launcher_name,
            version,
            channel,
            command,
            cwd,
        };

        self.run_start_request(request, json_flag)
    }

    pub(super) fn run_start_request(
        &self,
        request: StartRequest,
        json_flag: bool,
    ) -> Result<i32, String> {
        let existing = self.environment_service().find(&request.name)?;
        if existing.is_some() && request.root.is_some() {
            return Err(format!(
                "start cannot change the root for existing env {}; use env create or env clone for a new root",
                request.name
            ));
        }
        if existing.is_some() && request.gateway_port.is_some() {
            return Err(format!(
                "start cannot change the port for existing env {}; use a new env name or keep the current port",
                request.name
            ));
        }
        let created = existing.is_none();
        let onboarding_planned = matches!(request.onboarding_mode, StartOnboardingMode::Always);

        if json_flag && onboarding_planned {
            return Err(
                "start cannot combine --json with --onboard because onboarding is interactive"
                    .to_string(),
            );
        }

        if self.start_needs_official_release_host_check(existing.as_ref(), &request) {
            if let Some(code) = self.ensure_official_release_host_ready(None, json_flag)? {
                return Ok(code);
            }
            self.maybe_offer_git_install_for_repo_workflows(
                existing.is_none() && !json_flag && self.stdin_is_terminal(),
            )?;
        }

        let desired_binding = self.resolve_start_binding(
            &request.name,
            existing.as_ref(),
            request.runtime_name.clone(),
            request.launcher_name.clone(),
            request.version.clone(),
            request.channel.clone(),
            request.command.clone(),
            request.cwd.clone(),
        )?;

        if request.service_requested
            && let Some(error) = service_backend_support_error(&self.env)
        {
            return Err(error);
        }

        let mut meta = match existing {
            None => self
                .environment_service()
                .create(CreateEnvironmentOptions {
                    name: request.name.clone(),
                    root: request.root.clone(),
                    gateway_port: request.gateway_port,
                    service_enabled: request.service_requested,
                    service_running: request.service_requested && !onboarding_planned,
                    default_runtime: match desired_binding.as_ref() {
                        Some(StartBinding::Runtime(runtime_name)) => Some(runtime_name.clone()),
                        _ => None,
                    },
                    default_launcher: match desired_binding.as_ref() {
                        Some(StartBinding::Launcher(launcher_name)) => Some(launcher_name.clone()),
                        _ => None,
                    },
                    dev: None,
                    protected: request.protect,
                })?,
            Some(existing) => {
                self.apply_start_to_existing(existing, desired_binding.as_ref(), request.protect)?
            }
        };

        meta = self
            .environment_service()
            .apply_effective_gateway_port(meta)?;

        let (effective_port, gateway_port_source) = self
            .environment_service()
            .resolve_effective_gateway_port(&meta)?;

        if !onboarding_planned {
            ensure_minimum_local_openclaw_config(
                &derive_env_paths(Path::new(&meta.root)),
                effective_port,
            )?;
        }

        let detected_plain_home = if created {
            let source = inspect_migration_source(None, &self.env);
            source.exists.then_some(source.source_home)
        } else {
            None
        };
        let mut summary = StartSummary {
            env_name: request.name.clone(),
            created,
            root: meta.root.clone(),
            gateway_port: effective_port,
            gateway_port_source: gateway_port_source.to_string(),
            config_mode: if onboarding_planned {
                "onboarding".to_string()
            } else {
                "minimum".to_string()
            },
            default_runtime: meta.default_runtime.clone(),
            default_launcher: meta.default_launcher.clone(),
            protected: meta.protected,
            onboarding_planned,
            service_requested: request.service_requested,
            service_started: false,
            migrate_command: detected_plain_home
                .as_ref()
                .map(|_| format!("{} migrate <env>", self.command_example())),
            detected_plain_home,
            open_command: format!("{} @{} -- tui", self.command_example(), request.name),
            run_command: format!("{} @{} -- status", self.command_example(), request.name),
            onboard_command: if onboarding_planned && request.service_requested {
                format!(
                    "{} start {} --onboard",
                    self.command_example(),
                    request.name
                )
            } else {
                format!("{} @{} -- onboard", self.command_example(), request.name)
            },
            service_command: format!(
                "{} service install {}",
                self.command_example(),
                request.name
            ),
        };

        if onboarding_planned {
            self.stdout_lines(self.start_summary_lines(&summary));
            self.stdout_lines(self.onboarding_handoff_lines(&summary));
            let onboarding = self.handle_env_run(vec![
                request.name.clone(),
                "--".to_string(),
                "onboard".to_string(),
            ]);
            match onboarding {
                Ok(0) => {}
                Ok(code) => {
                    self.print_onboarding_follow_up(&summary, None, Some(code));
                    return Ok(code);
                }
                Err(error) => {
                    self.print_onboarding_follow_up(&summary, Some(&error), None);
                    return Ok(1);
                }
            }
        }

        if request.service_requested {
            self.with_progress(format!("Installing service for {}", request.name), || {
                self.service_service().install(&request.name)
            })?;
            self.with_progress(format!("Starting service for {}", request.name), || {
                self.service_service().start(&request.name)
            })?;
            summary.service_started = true;
        }

        if json_flag {
            self.print_json(&summary)?;
        } else if !onboarding_planned {
            self.stdout_lines(self.start_summary_lines(&summary));
        } else if summary.service_started {
            self.stdout_line(format!("Service running for env {}", request.name));
        }

        Ok(0)
    }

    fn start_needs_official_release_host_check(
        &self,
        existing: Option<&EnvMeta>,
        request: &StartRequest,
    ) -> bool {
        if request.version.is_some() || request.channel.is_some() {
            return true;
        }

        if request.runtime_name.is_some()
            || request.launcher_name.is_some()
            || request.command.is_some()
        {
            return false;
        }

        match existing {
            None => true,
            Some(env) => env.default_runtime.is_none() && env.default_launcher.is_none(),
        }
    }

    fn apply_start_to_existing(
        &self,
        mut meta: EnvMeta,
        desired_binding: Option<&StartBinding>,
        protect: bool,
    ) -> Result<EnvMeta, String> {
        if let Some(binding) = desired_binding {
            meta = match binding {
                StartBinding::Runtime(runtime_name)
                    if meta.default_runtime.as_deref() != Some(runtime_name.as_str())
                        || meta.default_launcher.is_some() =>
                {
                    self.environment_service()
                        .set_runtime(&meta.name, runtime_name)?
                }
                StartBinding::Launcher(launcher_name)
                    if meta.default_launcher.as_deref() != Some(launcher_name.as_str())
                        || meta.default_runtime.is_some() =>
                {
                    self.environment_service()
                        .set_launcher(&meta.name, launcher_name)?
                }
                _ => meta,
            };
        }

        if protect && !meta.protected {
            meta = self.environment_service().set_protected(&meta.name, true)?;
        }

        Ok(meta)
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_start_binding(
        &self,
        env_name: &str,
        existing: Option<&EnvMeta>,
        runtime_name: Option<String>,
        launcher_name: Option<String>,
        version: Option<String>,
        channel: Option<String>,
        command: Option<String>,
        cwd: Option<String>,
    ) -> Result<Option<StartBinding>, String> {
        if let Some(runtime_name) = runtime_name {
            let runtime_name = self
                .environment_service()
                .resolve_runtime_binding_request(
                    Some(validate_name(&runtime_name, "Runtime name")?),
                    None,
                    None,
                    "start",
                )?
                .expect("validated runtime binding request must resolve");
            return Ok(Some(StartBinding::Runtime(runtime_name)));
        }

        if let Some(launcher_name) = launcher_name {
            let launcher_name = validate_name(&launcher_name, "Launcher name")?;
            self.launcher_service().show(&launcher_name)?;
            return Ok(Some(StartBinding::Launcher(launcher_name)));
        }

        if let Some(command) = command {
            let launcher_name = self.ensure_start_launcher(env_name, &command, cwd.as_deref())?;
            return Ok(Some(StartBinding::Launcher(launcher_name)));
        }

        if version.is_some() || channel.is_some() {
            let runtime_name =
                self.with_progress(format!("Preparing OpenClaw runtime for {env_name}"), || {
                    self.environment_service()
                        .resolve_runtime_binding_request(None, version, channel, "start")
                })?;
            return Ok(runtime_name.map(StartBinding::Runtime));
        }

        if let Some(existing) = existing
            && (existing.default_runtime.is_some() || existing.default_launcher.is_some())
        {
            return Ok(None);
        }

        let runtime_name = self.with_progress(
            format!("Preparing latest stable OpenClaw for {env_name}"),
            || {
                self.environment_service().resolve_runtime_binding_request(
                    None,
                    None,
                    Some("stable".to_string()),
                    "start",
                )
            },
        )?;
        Ok(runtime_name.map(StartBinding::Runtime))
    }

    fn ensure_start_launcher(
        &self,
        env_name: &str,
        command: &str,
        cwd: Option<&str>,
    ) -> Result<String, String> {
        let launcher_name = format!("{env_name}.local");
        let desired_cwd = cwd
            .map(|raw| {
                resolve_absolute_path(raw, &self.env, &self.cwd).map(|path| display_path(&path))
            })
            .transpose()?;
        match self.launcher_service().show(&launcher_name) {
            Ok(existing) => {
                if existing.command == command && existing.cwd == desired_cwd {
                    Ok(launcher_name)
                } else {
                    Err(format!(
                        "launcher \"{launcher_name}\" already exists with different settings; use \"{} launcher add\" or choose another env name",
                        self.command_example()
                    ))
                }
            }
            Err(error) if error.contains("does not exist") => {
                self.launcher_service().add(AddLauncherOptions {
                    name: launcher_name.clone(),
                    command: command.to_string(),
                    cwd: cwd.map(str::to_string),
                    description: Some(format!("Local command for env {env_name}")),
                })?;
                Ok(launcher_name)
            }
            Err(error) => Err(error),
        }
    }

    fn start_summary_lines(&self, summary: &StartSummary) -> Vec<String> {
        if self.stdout_is_terminal() {
            return self.start_summary_lines_pretty(summary);
        }

        self.start_summary_lines_raw(summary)
    }

    fn start_summary_lines_raw(&self, summary: &StartSummary) -> Vec<String> {
        let mut lines = vec![if summary.created {
            format!("Started env {}", summary.env_name)
        } else {
            format!("Using env {}", summary.env_name)
        }];
        lines.push(format!("  root: {}", summary.root));
        lines.push(format!(
            "  port: {} ({})",
            summary.gateway_port, summary.gateway_port_source
        ));
        if let Some(runtime) = summary.default_runtime.as_deref() {
            lines.push(format!("  runtime: {runtime}"));
        }
        if let Some(launcher) = summary.default_launcher.as_deref() {
            lines.push(format!("  launcher: {launcher}"));
        }
        if summary.protected {
            lines.push("  protected: true".to_string());
        }
        if summary.service_requested {
            lines.push(format!(
                "  service: {}",
                if summary.service_started {
                    "running"
                } else {
                    "requested"
                }
            ));
        } else {
            lines.push(format!("  service: {}", summary.service_command));
        }
        if summary.onboarding_planned {
            lines.push("  onboarding: running now".to_string());
        } else {
            lines.push(format!(
                "  config: {}",
                if summary.config_mode == "minimum" {
                    "minimum local"
                } else {
                    &summary.config_mode
                }
            ));
            lines.push(format!("  onboard: {}", summary.onboard_command));
        }
        if let Some(plain_home) = summary.detected_plain_home.as_deref() {
            lines.push(format!("  existing plain home: {plain_home}"));
            if let Some(command) = summary.migrate_command.as_deref() {
                lines.push(format!("  migrate instead: {command}"));
            }
        }
        lines.push(format!("  open: {}", summary.open_command));
        lines.push(format!("  run: {}", summary.run_command));
        lines
    }

    fn start_summary_lines_pretty(&self, summary: &StartSummary) -> Vec<String> {
        let color = self.color_output_enabled_for(self.stdout_is_terminal(), self.color_mode());
        let mut lines = vec![paint(
            if summary.onboarding_planned {
                if summary.created {
                    "Environment ready"
                } else {
                    "Environment updated"
                }
            } else if summary.created {
                "OpenClaw ready"
            } else {
                "OpenClaw updated"
            },
            Tone::Strong,
            color,
        )];
        lines.push(String::new());

        let mut overview = vec![
            KeyValueRow::accent("Env", &summary.env_name),
            KeyValueRow::plain("Root", &summary.root),
            KeyValueRow::accent(
                "Port",
                format!("{} ({})", summary.gateway_port, summary.gateway_port_source),
            ),
        ];
        if let Some(runtime) = summary.default_runtime.as_deref() {
            overview.push(KeyValueRow::success("Runtime", runtime));
        }
        if let Some(launcher) = summary.default_launcher.as_deref() {
            overview.push(KeyValueRow::success("Launcher", launcher));
        }
        overview.push(KeyValueRow::plain(
            "Service",
            if summary.service_requested {
                if summary.service_started {
                    "running".to_string()
                } else {
                    "requested".to_string()
                }
            } else {
                "not installed".to_string()
            },
        ));
        overview.push(KeyValueRow::plain(
            "Config",
            if summary.config_mode == "minimum" {
                "minimum local".to_string()
            } else {
                summary.config_mode.clone()
            },
        ));
        lines.extend(render_key_value_card("Environment", &overview, color));
        lines.push(String::new());

        if summary.onboarding_planned {
            let mut up_next = vec![
                KeyValueRow::accent("OpenClaw", "Onboarding starts below"),
                KeyValueRow::warning("Retry", &summary.onboard_command),
                KeyValueRow::accent("Status later", &summary.run_command),
            ];
            if !summary.service_requested {
                up_next.push(KeyValueRow::muted("Keep running", &summary.service_command));
            }
            lines.extend(render_key_value_card("Up next", &up_next, color));
        } else {
            let mut next = vec![
                KeyValueRow::accent("Open", &summary.open_command),
                KeyValueRow::accent("Status", &summary.run_command),
            ];
            next.push(KeyValueRow::warning("Onboard", &summary.onboard_command));
            if !summary.service_requested {
                next.push(KeyValueRow::muted("Keep running", &summary.service_command));
            }
            lines.extend(render_key_value_card("Next", &next, color));
        }
        if let Some(plain_home) = summary.detected_plain_home.as_deref() {
            let mut existing = vec![KeyValueRow::warning("Detected plain home", plain_home)];
            if let Some(command) = summary.migrate_command.as_deref() {
                existing.push(KeyValueRow::muted("Bring it under OCM", command));
            }
            lines.push(String::new());
            lines.extend(render_key_value_card(
                "Already Using OpenClaw?",
                &existing,
                color,
            ));
        }
        lines
    }

    fn onboarding_handoff_lines(&self, summary: &StartSummary) -> Vec<String> {
        if !self.stdout_is_terminal() {
            return Vec::new();
        }

        let color = self.color_output_enabled_for(self.stdout_is_terminal(), self.color_mode());
        onboarding_handoff_lines_pretty(summary, color)
    }

    fn print_onboarding_follow_up(
        &self,
        summary: &StartSummary,
        error: Option<&str>,
        exit_code: Option<i32>,
    ) {
        match (error, exit_code) {
            (Some(error), _) => {
                self.stderr_line(format!(
                    "ocm: env {} is ready, but onboarding could not start",
                    summary.env_name
                ));
                self.stderr_line(format!("  problem: {error}"));
            }
            (None, Some(code)) => {
                self.stderr_line(format!(
                    "ocm: env {} is ready, but onboarding exited with code {code}",
                    summary.env_name
                ));
            }
            _ => return,
        }

        self.stderr_line(format!("  retry: {}", summary.onboard_command));
        self.stderr_line(format!("  run: {}", summary.run_command));
        if summary.service_requested {
            self.stderr_line(format!(
                "  service: {} service status {}",
                self.command_example(),
                summary.env_name
            ));
        } else {
            self.stderr_line(format!("  keep running: {}", summary.service_command));
        }
    }
}

fn onboarding_handoff_lines_pretty(summary: &StartSummary, color: bool) -> Vec<String> {
    vec![
        String::new(),
        paint("OpenClaw onboarding starts below.", Tone::Accent, color),
        paint(
            &format!("If you stop now, rerun: {}", summary.onboard_command),
            Tone::Muted,
            color,
        ),
        String::new(),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::cli::INTERNAL_COLOR_MODE_ENV;

    use super::{Cli, StartSummary, onboarding_handoff_lines_pretty};

    fn sample_summary(onboarding_planned: bool) -> StartSummary {
        StartSummary {
            env_name: "mira".to_string(),
            created: true,
            root: "/tmp/mira".to_string(),
            gateway_port: 18_789,
            gateway_port_source: "metadata".to_string(),
            config_mode: if onboarding_planned {
                "onboarding".to_string()
            } else {
                "minimum".to_string()
            },
            default_runtime: Some("stable".to_string()),
            default_launcher: None,
            protected: false,
            onboarding_planned,
            service_requested: true,
            service_started: true,
            detected_plain_home: None,
            migrate_command: None,
            open_command: "ocm @mira -- tui".to_string(),
            run_command: "ocm @mira -- status".to_string(),
            onboard_command: "ocm @mira -- onboard".to_string(),
            service_command: "ocm service install mira".to_string(),
        }
    }

    fn test_cli() -> Cli {
        let mut env = BTreeMap::new();
        env.insert(INTERNAL_COLOR_MODE_ENV.to_string(), "never".to_string());
        Cli {
            env,
            cwd: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn pretty_start_summary_for_onboarding_flow_uses_up_next_card() {
        let lines = test_cli().start_summary_lines_pretty(&sample_summary(true));
        assert_eq!(lines[0], "Environment ready");
        assert!(lines.iter().any(|line| line.contains("Environment")));
        assert!(lines.iter().any(|line| line.contains("Runtime")));
        assert!(lines.iter().any(|line| line.contains("Up next")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Onboarding starts below"))
        );
        assert!(!lines.iter().any(|line| line.contains("Activate")));
    }

    #[test]
    fn pretty_start_summary_without_onboarding_keeps_next_steps() {
        let lines = test_cli().start_summary_lines_pretty(&sample_summary(false));
        assert_eq!(lines[0], "OpenClaw ready");
        assert!(lines.iter().any(|line| line.contains("Next")));
        assert!(lines.iter().any(|line| line.contains("Open")));
        assert!(lines.iter().any(|line| line.contains("ocm @mira -- tui")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm @mira -- status"))
        );
    }

    #[test]
    fn onboarding_handoff_mentions_retry_command() {
        let lines = onboarding_handoff_lines_pretty(&sample_summary(true), false);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("OpenClaw onboarding starts below"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm @mira -- onboard"))
        );
    }
}
