use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{EnvMeta, EnvironmentService};
use crate::infra::shell::{build_openclaw_dev_source_env, build_openclaw_env};
use crate::launcher::{
    build_launcher_command, resolve_direct_launcher_command, resolve_launcher_run_dir,
};
use crate::managed_node::apply_path_prepend_to_environment;
use crate::runtime::resolve_runtime_launch;
use crate::store::{display_path, get_launcher, get_launcher_verified, get_runtime_verified};

use super::SourceWatchOverride;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSummary {
    pub env_name: String,
    pub binding_kind: String,
    pub binding_name: String,
    pub command: Option<String>,
    pub binary_path: Option<String>,
    pub runtime_source_kind: Option<String>,
    pub runtime_release_version: Option<String>,
    pub runtime_release_channel: Option<String>,
    pub forwarded_args: Vec<String>,
    pub run_dir: String,
}

#[derive(Clone, Debug)]
pub struct GatewayProcessSpec {
    pub env_name: String,
    pub binding_kind: String,
    pub binding_name: String,
    pub command: Option<String>,
    pub binary_path: Option<String>,
    pub runtime_source_kind: Option<String>,
    pub runtime_release_version: Option<String>,
    pub runtime_release_channel: Option<String>,
    pub args: Vec<String>,
    pub run_dir: PathBuf,
    pub process_env: BTreeMap<String, String>,
}

#[derive(Debug)]
pub enum ExecutionBinding {
    Launcher(String),
    Runtime(String),
    Dev,
}

pub fn resolve_execution_binding(
    env_meta: &EnvMeta,
    runtime_override: Option<String>,
    launcher_override: Option<String>,
) -> Result<ExecutionBinding, String> {
    let runtime_override = runtime_override.filter(|value| !value.trim().is_empty());
    let launcher_override = launcher_override.filter(|value| !value.trim().is_empty());

    if runtime_override.is_some() && launcher_override.is_some() {
        return Err("env run accepts only one of --runtime or --launcher".to_string());
    }

    if let Some(runtime_name) = runtime_override {
        return Ok(ExecutionBinding::Runtime(runtime_name));
    }

    if let Some(launcher_name) = launcher_override {
        return Ok(ExecutionBinding::Launcher(launcher_name));
    }

    if let Some(runtime_name) = env_meta.default_runtime.clone() {
        return Ok(ExecutionBinding::Runtime(runtime_name));
    }

    if let Some(launcher_name) = env_meta.default_launcher.clone() {
        return Ok(ExecutionBinding::Launcher(launcher_name));
    }

    if env_meta.dev.is_some() {
        return Ok(ExecutionBinding::Dev);
    }

    Err(format!(
        "environment \"{}\" has no default runtime, launcher, or dev binding; use ocm dev <name>, env set-runtime, env set-launcher, or pass --runtime/--launcher",
        env_meta.name
    ))
}

/// Read configuration with the environment and cwd used by ordinary target startup.
/// This does not execute the binding or acquire authority from config.env.
pub(crate) fn resolve_config_read_context(
    meta: &EnvMeta,
    process_env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<crate::store::OpenClawConfigReadContext, String> {
    // A target can be built before it has a runtime binding. Its isolated root
    // remains the default launch directory; do not require a runnable artifact.
    if meta.default_runtime.is_none() && meta.default_launcher.is_none() && meta.dev.is_none() {
        return crate::store::OpenClawConfigReadContext::for_launch(
            &build_openclaw_env(meta, process_env),
            Path::new(&meta.root),
        );
    }
    let binding = resolve_execution_binding(meta, None, None)?;
    config_read_context_for_binding(meta, process_env, cwd, binding)
}

/// Read configuration in an explicitly selected binding's launch context.
pub(crate) fn config_read_context_for_binding(
    meta: &EnvMeta,
    process_env: &BTreeMap<String, String>,
    cwd: &Path,
    binding: ExecutionBinding,
) -> Result<crate::store::OpenClawConfigReadContext, String> {
    let (env, cwd) = match binding {
        crate::env::ExecutionBinding::Dev => {
            let source = meta
                .dev
                .as_ref()
                .ok_or("dev binding is missing")?
                .execution_source_root()?;
            (
                crate::infra::shell::build_openclaw_dev_source_env(meta, process_env, source),
                source.to_path_buf(),
            )
        }
        crate::env::ExecutionBinding::Runtime(_) => (
            crate::infra::shell::build_openclaw_env(meta, process_env),
            Path::new(&meta.root).to_path_buf(),
        ),
        crate::env::ExecutionBinding::Launcher(name) => {
            let launcher = crate::store::get_launcher(&name, process_env, cwd)?;
            (
                crate::infra::shell::build_openclaw_env(meta, process_env),
                crate::launcher::resolve_launcher_run_dir(&launcher, Path::new(&meta.root)),
            )
        }
    };
    crate::store::OpenClawConfigReadContext::for_launch(&env, &cwd)
}

pub fn resolve_runtime_run_dir(fallback_cwd: &Path) -> PathBuf {
    fallback_cwd.to_path_buf()
}

fn gateway_shell_program_arguments(
    command: &str,
    process_env: &BTreeMap<String, String>,
) -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd".to_string(), "/C".to_string(), command.to_string()]
    } else {
        vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            crate::infra::shell::render_env_scoped_login_command(command, process_env),
        ]
    }
}

pub fn resolve_gateway_process_spec(
    env_meta: &EnvMeta,
    process_env: &BTreeMap<String, String>,
    cwd: &Path,
    bootstrap_managed_node: bool,
) -> Result<GatewayProcessSpec, String> {
    let port = env_meta.gateway_port.ok_or_else(|| {
        format!(
            "failed to resolve gateway port for env \"{}\"",
            env_meta.name
        )
    })?;
    let gateway_args = vec![
        "gateway".to_string(),
        "run".to_string(),
        "--port".to_string(),
        port.to_string(),
    ];

    match resolve_execution_binding(env_meta, None, None)? {
        ExecutionBinding::Launcher(binding_name) => {
            // The supervisor plans and retries a launcher whose directory is
            // missing, so it can recover once the directory is back; commands
            // that start or resolve it refuse it up front instead.
            let launcher = get_launcher(&binding_name, process_env, cwd)?;
            let run_dir = resolve_launcher_run_dir(&launcher, Path::new(&env_meta.root));
            let direct = resolve_direct_launcher_command(&launcher, &gateway_args, &run_dir);
            Ok(GatewayProcessSpec {
                env_name: env_meta.name.clone(),
                binding_kind: "launcher".to_string(),
                binding_name,
                command: Some(build_launcher_command(&launcher, &gateway_args)),
                binary_path: direct.as_ref().map(|command| command.program.clone()),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                args: direct.map(|command| command.args).unwrap_or_default(),
                run_dir,
                process_env: build_openclaw_env(env_meta, process_env),
            })
        }
        ExecutionBinding::Runtime(binding_name) => {
            let runtime = get_runtime_verified(&binding_name, process_env, cwd)?;
            let launch = resolve_runtime_launch(
                &runtime,
                &gateway_args,
                process_env,
                cwd,
                bootstrap_managed_node,
            )?;
            let mut process_env = build_openclaw_env(env_meta, process_env);
            apply_path_prepend_to_environment(&mut process_env, launch.path_prepend.as_deref())?;
            Ok(GatewayProcessSpec {
                env_name: env_meta.name.clone(),
                binding_kind: "runtime".to_string(),
                binding_name,
                command: None,
                binary_path: Some(launch.program),
                runtime_source_kind: Some(runtime.source_kind.as_str().to_string()),
                runtime_release_version: runtime.release_version.clone(),
                runtime_release_channel: runtime.release_channel.clone(),
                args: launch.args,
                run_dir: Path::new(&env_meta.root).to_path_buf(),
                process_env,
            })
        }
        ExecutionBinding::Dev => {
            let dev = env_meta.dev.as_ref().ok_or_else(|| {
                format!(
                    "environment \"{}\" is missing its dev binding",
                    env_meta.name
                )
            })?;
            let source_root = dev.execution_source_root()?;
            let mut args = vec!["openclaw".to_string()];
            args.extend(gateway_args);
            Ok(GatewayProcessSpec {
                env_name: env_meta.name.clone(),
                binding_kind: "dev".to_string(),
                binding_name: "dev".to_string(),
                command: Some(format!("pnpm {}", args.join(" "))),
                binary_path: Some("pnpm".to_string()),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                args,
                run_dir: source_root.to_path_buf(),
                process_env: build_openclaw_dev_source_env(env_meta, process_env, source_root),
            })
        }
    }
}

/// Yields the OpenClaw command words in forwarded arguments, such as `gateway`
/// and `stop`, the way OpenClaw finds its command path: root options are
/// skipped with their values (`--no-color`, `--log-level debug`), other flags
/// are skipped, a leading `--` makes the following tokens literal words, and a
/// `--` after a command word ends the command.
pub(crate) fn openclaw_command_words(args: &[String]) -> impl Iterator<Item = &str> {
    openclaw_command_path(args).map(|(word, _)| word)
}

/// Like `openclaw_command_words`, also telling whether each word follows a
/// leading `--`, where OpenClaw passes the remaining tokens as operands rather
/// than options.
fn openclaw_command_path(args: &[String]) -> impl Iterator<Item = (&str, bool)> {
    let mut index = 0;
    let mut literal = false;
    let mut found = false;
    std::iter::from_fn(move || {
        while let Some(arg) = args.get(index).map(String::as_str) {
            index += 1;
            if !literal {
                if arg == "--" {
                    if found {
                        index = args.len();
                        return None;
                    }
                    literal = true;
                    continue;
                }
                if OPENCLAW_ROOT_OPTIONS_WITH_VALUE.contains(&arg) {
                    // OpenClaw requires a value here and takes the next token as it is.
                    index = (index + 1).min(args.len());
                    continue;
                }
                if arg.starts_with('-') {
                    continue;
                }
            }
            found = true;
            return Some((arg, literal));
        }
        None
    })
}

const OPENCLAW_ROOT_OPTIONS_WITH_VALUE: [&str; 3] = ["--profile", "--log-level", "--container"];

/// Yields the flags before the first `--` in forwarded arguments, such as
/// `--help` or `--yes`, leaving out OpenClaw's root options (`--no-color`,
/// `--log-level debug`) and their values.
pub(crate) fn openclaw_options(args: &[String]) -> impl Iterator<Item = &str> {
    let mut index = 0;
    std::iter::from_fn(move || {
        while let Some(arg) = args.get(index).map(String::as_str) {
            if arg == "--" {
                index = args.len();
                return None;
            }
            let root_tokens = openclaw_root_option_tokens(args, index);
            index += root_tokens.max(1);
            if root_tokens == 0 && arg.starts_with('-') {
                return Some(arg);
            }
        }
        None
    })
}

/// Counts the tokens an OpenClaw root option at `index` takes, or 0 when the
/// token is not a root option. As in OpenClaw's help detection, a root option
/// takes the next token as its value only when that token does not look like a
/// flag.
fn openclaw_root_option_tokens(args: &[String], index: usize) -> usize {
    let Some(arg) = args.get(index).map(String::as_str) else {
        return 0;
    };
    if OPENCLAW_ROOT_OPTIONS_WITH_VALUE.contains(&arg) {
        let has_value = args
            .get(index + 1)
            .is_some_and(|value| !value.starts_with('-'));
        return 1 + usize::from(has_value);
    }
    let with_inline_value = OPENCLAW_ROOT_OPTIONS_WITH_VALUE.iter().any(|option| {
        arg.strip_prefix(option)
            .is_some_and(|rest| rest.starts_with('='))
    });
    usize::from(matches!(arg, "--dev" | "--no-color") || with_inline_value)
}

/// Tells whether forwarded arguments run `onboard recommendations`, which
/// reads or clears stored app recommendations rather than onboarding: `onboard`
/// followed by its `recommendations` subcommand after only OpenClaw's root
/// options and `--json`, the one `onboard` option that subcommand accepts.
/// After any other option the word can be that option's value, as in
/// `onboard --workspace recommendations`, which still onboards.
pub(crate) fn names_onboard_recommendations(args: &[String]) -> bool {
    let mut seen_onboard = false;
    let mut index = 0;
    while let Some(arg) = args.get(index).map(String::as_str) {
        let root_tokens = openclaw_root_option_tokens(args, index);
        if root_tokens > 0 {
            index += root_tokens;
            continue;
        }
        index += 1;
        match arg {
            "onboard" if !seen_onboard => seen_onboard = true,
            "recommendations" if seen_onboard => return true,
            "--json" if seen_onboard => {}
            _ if !seen_onboard && arg.starts_with('-') => {}
            _ => return false,
        }
    }
    false
}

fn normalize_openclaw_args_for_env(args: &[String]) -> Result<Vec<String>, String> {
    let Some(("onboard", after_terminator)) = openclaw_command_path(args).next() else {
        return Ok(args.to_vec());
    };

    if names_onboard_recommendations(args) {
        return Ok(args.to_vec());
    }

    if args.iter().any(|arg| arg == "--install-daemon") {
        return Err(
            "env onboarding cannot install the OpenClaw daemon because the daemon service is global; rerun with --no-install-daemon or --skip-daemon, or run onboard outside OCM".to_string(),
        );
    }

    if args
        .iter()
        .any(|arg| arg == "--no-install-daemon" || arg == "--skip-daemon")
    {
        return Ok(args.to_vec());
    }

    if after_terminator {
        return Err(
            "env onboarding cannot add --no-install-daemon when \"--\" comes before onboard, because OpenClaw then reads it as an argument; remove that \"--\" or run onboard outside OCM".to_string(),
        );
    }

    let mut normalized = args.to_vec();
    normalized.push("--no-install-daemon".to_string());
    Ok(normalized)
}

pub enum ResolvedExecution {
    Launcher {
        env: EnvMeta,
        launcher_name: String,
        command: String,
        run_dir: PathBuf,
    },
    Runtime {
        env: EnvMeta,
        runtime_name: String,
        binary_path: String,
        runtime_source_kind: String,
        runtime_release_version: Option<String>,
        runtime_release_channel: Option<String>,
        forwarded_args: Vec<String>,
        program: String,
        program_args: Vec<String>,
        path_prepend: Option<PathBuf>,
        run_dir: PathBuf,
    },
    Dev {
        env: EnvMeta,
        repo_root: String,
        worktree_root: String,
        forwarded_args: Vec<String>,
        program: String,
        program_args: Vec<String>,
        run_dir: PathBuf,
    },
    SourceWatch {
        env: EnvMeta,
        source: SourceWatchOverride,
        forwarded_args: Vec<String>,
        program: String,
        program_args: Vec<String>,
        run_dir: PathBuf,
    },
}

impl GatewayProcessSpec {
    pub fn program_arguments(&self) -> Vec<String> {
        match (&self.binary_path, &self.command) {
            (Some(binary_path), _) => {
                let mut program_arguments = vec![binary_path.clone()];
                program_arguments.extend(self.args.iter().cloned());
                program_arguments
            }
            (None, Some(command)) => gateway_shell_program_arguments(command, &self.process_env),
            (None, None) => Vec::new(),
        }
    }

    pub fn restart_handoff_pid_bound(&self) -> bool {
        let Some(binary_path) = self.binary_path.as_deref() else {
            return false;
        };
        if is_openclaw_entrypoint(binary_path) {
            return true;
        }

        let Some(openclaw_entrypoint) = self.args.first().map(String::as_str) else {
            return false;
        };
        is_openclaw_entrypoint(openclaw_entrypoint)
            && self.binding_kind == "runtime"
            && self.runtime_source_kind.as_deref() == Some("installed")
            && is_node_binary(binary_path)
    }
}

fn is_openclaw_entrypoint(path: &str) -> bool {
    Path::new(path).file_name().and_then(|name| name.to_str()) == Some("openclaw.mjs")
}

fn is_node_binary(binary_path: &str) -> bool {
    matches!(
        Path::new(binary_path)
            .file_name()
            .and_then(|name| name.to_str()),
        Some("node" | "node.exe")
    )
}

impl<'a> EnvironmentService<'a> {
    pub fn resolve(
        &self,
        name: &str,
        runtime_override: Option<String>,
        launcher_override: Option<String>,
        args: &[String],
    ) -> Result<ResolvedExecution, String> {
        self.resolve_execution(self.get(name)?, runtime_override, launcher_override, args)
    }

    pub(crate) fn resolve_simulation(
        &self,
        expected: &EnvMeta,
        args: &[String],
    ) -> Result<(ResolvedExecution, crate::store::EnvironmentOperationLock), String> {
        let (current, lock) = self.lock_simulation(expected)?;
        let resolved = self.resolve_execution(current, None, None, args)?;
        Ok((resolved, lock))
    }

    pub fn resolve_run(
        &self,
        name: &str,
        runtime_override: Option<String>,
        launcher_override: Option<String>,
        args: &[String],
    ) -> Result<ResolvedExecution, String> {
        self.resolve_execution(self.touch(name)?, runtime_override, launcher_override, args)
    }

    pub(crate) fn with_resolved_run<T>(
        &self,
        name: &str,
        runtime_override: Option<String>,
        launcher_override: Option<String>,
        args: &[String],
        run: impl FnOnce(ResolvedExecution) -> Result<T, String>,
    ) -> Result<T, String> {
        // Retain only the selected runtime's custody, from resolution through
        // foreground completion. touch releases environment admission first.
        let mut runtime_guard = None;
        let resolved = self.resolve_execution_with_runtime_custody(
            self.touch(name)?,
            runtime_override,
            launcher_override,
            args,
            |name| {
                runtime_guard = Some(crate::store::try_lock_runtime_binding(
                    name, self.env, self.cwd,
                )?);
                Ok(())
            },
        )?;
        let result = run(resolved);
        drop(runtime_guard);
        result
    }

    pub fn resolve_gateway_process(
        &self,
        name: &str,
        bootstrap_managed_node: bool,
    ) -> Result<GatewayProcessSpec, String> {
        let env = self.get(name)?;
        if let Some(source) = self.active_source_watch_override(&env.name)? {
            let env = self.source_watch_environment(env, &source)?;
            return source_watch_gateway_process_spec(&env, self.env, source);
        }
        let env = self.apply_effective_gateway_port(env)?;
        resolve_gateway_process_spec(&env, self.env, self.cwd, bootstrap_managed_node)
    }

    pub(crate) fn source_watch_environment(
        &self,
        mut env: EnvMeta,
        source: &SourceWatchOverride,
    ) -> Result<EnvMeta, String> {
        if let Some(dev) = &env.dev {
            dev.execution_source_root()?;
        }
        let Some(endpoint) = &source.endpoint else {
            return self.apply_effective_gateway_port(env);
        };
        if env.root != endpoint.env_root {
            return Err(format!(
                "source watch for env {} was launched with root {}; refusing to route commands through the changed env root",
                env.name, endpoint.env_root
            ));
        }
        env.gateway_port = Some(endpoint.gateway_port);
        Ok(env)
    }

    fn resolve_execution(
        &self,
        env: EnvMeta,
        runtime_override: Option<String>,
        launcher_override: Option<String>,
        args: &[String],
    ) -> Result<ResolvedExecution, String> {
        self.resolve_execution_with_runtime_custody(
            env,
            runtime_override,
            launcher_override,
            args,
            |_| Ok(()),
        )
    }

    fn resolve_execution_with_runtime_custody(
        &self,
        env: EnvMeta,
        runtime_override: Option<String>,
        launcher_override: Option<String>,
        args: &[String],
        retain_runtime: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<ResolvedExecution, String> {
        let args = normalize_openclaw_args_for_env(args)?;
        let has_runtime_override = runtime_override
            .as_ref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
        let has_launcher_override = launcher_override
            .as_ref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);

        if has_runtime_override && has_launcher_override {
            return Err("env run accepts only one of --runtime or --launcher".to_string());
        }

        if !has_runtime_override
            && !has_launcher_override
            && let Some(source) = self.active_source_watch_override(&env.name)?
        {
            let env = self.source_watch_environment(env, &source)?;
            let program_args = source_watch_openclaw_program_args(&args);
            let run_dir = PathBuf::from(&source.repo_root);
            return Ok(ResolvedExecution::SourceWatch {
                env,
                source,
                forwarded_args: args,
                program: "node".to_string(),
                program_args,
                run_dir,
            });
        }

        let env = self.apply_effective_gateway_port(env)?;
        match resolve_execution_binding(&env, runtime_override, launcher_override)? {
            ExecutionBinding::Launcher(launcher_name) => {
                let launcher = get_launcher_verified(&launcher_name, self.env, self.cwd)?;
                Ok(ResolvedExecution::Launcher {
                    launcher_name,
                    command: build_launcher_command(&launcher, &args),
                    run_dir: resolve_launcher_run_dir(&launcher, self.cwd),
                    env,
                })
            }
            ExecutionBinding::Runtime(runtime_name) => {
                retain_runtime(&runtime_name)?;
                let runtime = get_runtime_verified(&runtime_name, self.env, self.cwd)?;
                let launch = resolve_runtime_launch(&runtime, &args, self.env, self.cwd, true)?;
                Ok(ResolvedExecution::Runtime {
                    runtime_name,
                    binary_path: launch.runtime_binary_path,
                    runtime_source_kind: runtime.source_kind.as_str().to_string(),
                    runtime_release_version: runtime.release_version.clone(),
                    runtime_release_channel: runtime.release_channel.clone(),
                    forwarded_args: args,
                    program: launch.program,
                    program_args: launch.args,
                    path_prepend: launch.path_prepend,
                    run_dir: resolve_runtime_run_dir(self.cwd),
                    env,
                })
            }
            ExecutionBinding::Dev => {
                let dev = env.dev.clone().ok_or_else(|| {
                    format!("environment \"{}\" is missing its dev binding", env.name)
                })?;
                let run_dir = dev.execution_source_root()?.to_path_buf();
                let mut program_args = vec!["openclaw".to_string()];
                program_args.extend(args.clone());
                Ok(ResolvedExecution::Dev {
                    env,
                    repo_root: dev.repo_root().to_string(),
                    worktree_root: dev.source_root().to_string(),
                    forwarded_args: args,
                    program: "pnpm".to_string(),
                    program_args,
                    run_dir,
                })
            }
        }
    }
}

impl ResolvedExecution {
    pub fn into_summary(self) -> ExecutionSummary {
        match self {
            Self::Launcher {
                env,
                launcher_name,
                command,
                run_dir,
            } => ExecutionSummary {
                env_name: env.name,
                binding_kind: "launcher".to_string(),
                binding_name: launcher_name,
                command: Some(command),
                binary_path: None,
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                forwarded_args: Vec::new(),
                run_dir: run_dir.display().to_string(),
            },
            Self::Runtime {
                env,
                runtime_name,
                binary_path,
                runtime_source_kind,
                runtime_release_version,
                runtime_release_channel,
                forwarded_args,
                run_dir,
                ..
            } => ExecutionSummary {
                env_name: env.name,
                binding_kind: "runtime".to_string(),
                binding_name: runtime_name,
                command: None,
                binary_path: Some(binary_path),
                runtime_source_kind: Some(runtime_source_kind),
                runtime_release_version,
                runtime_release_channel,
                forwarded_args,
                run_dir: run_dir.display().to_string(),
            },
            Self::Dev {
                env,
                repo_root,
                worktree_root: _,
                forwarded_args,
                run_dir,
                ..
            } => ExecutionSummary {
                env_name: env.name,
                binding_kind: "dev".to_string(),
                binding_name: "dev".to_string(),
                command: Some(format!("pnpm openclaw ({repo_root})")),
                binary_path: Some("pnpm".to_string()),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                forwarded_args,
                run_dir: run_dir.display().to_string(),
            },
            Self::SourceWatch {
                env,
                source,
                forwarded_args,
                run_dir,
                ..
            } => ExecutionSummary {
                env_name: env.name,
                binding_kind: "source-watch".to_string(),
                binding_name: "source-watch".to_string(),
                command: Some(source_watch_command_label(&source, &forwarded_args)),
                binary_path: Some(display_path(&source.openclaw_entry_path())),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                forwarded_args,
                run_dir: run_dir.display().to_string(),
            },
        }
    }
}

fn source_watch_gateway_process_spec(
    env_meta: &EnvMeta,
    process_env: &BTreeMap<String, String>,
    source: SourceWatchOverride,
) -> Result<GatewayProcessSpec, String> {
    let port = env_meta.gateway_port.ok_or_else(|| {
        format!(
            "failed to resolve gateway port for env \"{}\"",
            env_meta.name
        )
    })?;
    let gateway_args = vec![
        "gateway".to_string(),
        "run".to_string(),
        "--port".to_string(),
        port.to_string(),
    ];
    let args = source_watch_openclaw_program_args(&gateway_args);

    Ok(GatewayProcessSpec {
        env_name: env_meta.name.clone(),
        binding_kind: "source-watch".to_string(),
        binding_name: "source-watch".to_string(),
        command: Some(source_watch_command_label(&source, &gateway_args)),
        binary_path: Some("node".to_string()),
        runtime_source_kind: None,
        runtime_release_version: None,
        runtime_release_channel: None,
        args,
        run_dir: PathBuf::from(&source.repo_root),
        process_env: build_openclaw_dev_source_env(
            env_meta,
            process_env,
            Path::new(&source.repo_root),
        ),
    })
}

pub(crate) fn source_watch_openclaw_program_args(openclaw_args: &[String]) -> Vec<String> {
    // Every source caller pins cwd to its selected checkout. Hardened Node on
    // Windows rejects canonical extended absolute main paths at the drive.
    let mut program_args = vec!["./openclaw.mjs".to_string()];
    program_args.extend(openclaw_args.iter().cloned());
    program_args
}

fn source_watch_command_label(source: &SourceWatchOverride, openclaw_args: &[String]) -> String {
    let mut parts = vec![source.command_label()];
    parts.extend(openclaw_args.iter().cloned());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::{normalize_openclaw_args_for_env, openclaw_command_words};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn command_words_skip_openclaw_root_options() {
        let cases: [(&[&str], &[&str]); 11] = [
            (&["onboard"], &["onboard"]),
            (&["--no-color", "onboard"], &["onboard"]),
            (&["--dev", "--log-level", "debug", "onboard"], &["onboard"]),
            (
                &["--log-level=debug", "gateway", "stop"],
                &["gateway", "stop"],
            ),
            (
                &["--profile", "work", "--container", "box", "setup"],
                &["setup"],
            ),
            (
                &["--log-level", "--no-color", "gateway", "stop"],
                &["gateway", "stop"],
            ),
            (&["--profile", "--", "onboard"], &["onboard"]),
            (
                &["--", "onboard", "--install-daemon"],
                &["onboard", "--install-daemon"],
            ),
            (&["--log-level", "debug", "--", "onboard"], &["onboard"]),
            (&["onboard", "--", "setup"], &["onboard"]),
            (&["--log-level"], &[]),
        ];
        for (forwarded, expected) in cases {
            assert_eq!(
                openclaw_command_words(&args(forwarded)).collect::<Vec<_>>(),
                expected,
                "{forwarded:?}"
            );
        }
    }

    #[test]
    fn onboarding_normalization_applies_after_root_options() {
        assert_eq!(
            normalize_openclaw_args_for_env(&args(&["--log-level", "debug", "onboard"])).unwrap(),
            args(&["--log-level", "debug", "onboard", "--no-install-daemon"])
        );
        assert_eq!(
            normalize_openclaw_args_for_env(&args(&["--no-color", "onboard", "--skip-daemon"]))
                .unwrap(),
            args(&["--no-color", "onboard", "--skip-daemon"])
        );
        assert_eq!(
            normalize_openclaw_args_for_env(&args(&["--profile", "--", "onboard"])).unwrap(),
            args(&["--profile", "--", "onboard", "--no-install-daemon"])
        );
        for forwarded in [
            &["--", "onboard"][..],
            &["--log-level", "debug", "--", "onboard"],
        ] {
            let error = normalize_openclaw_args_for_env(&args(forwarded)).unwrap_err();
            assert!(
                error.contains("when \"--\" comes before onboard"),
                "{forwarded:?}: {error}"
            );
        }
        let refused: [&[&str]; 5] = [
            &["--no-color", "onboard", "--install-daemon"],
            &["--", "onboard", "--install-daemon"],
            &["--log-level", "debug", "--", "onboard", "--install-daemon"],
            &["--log-level=debug", "onboard", "--install-daemon"],
            &["--profile", "work", "onboard", "--install-daemon"],
        ];
        for forwarded in refused {
            let error = normalize_openclaw_args_for_env(&args(forwarded)).unwrap_err();
            assert!(
                error.contains("the daemon service is global"),
                "{forwarded:?}: {error}"
            );
        }
        assert_eq!(
            normalize_openclaw_args_for_env(&args(&["--log-level", "debug", "status"])).unwrap(),
            args(&["--log-level", "debug", "status"])
        );
    }

    #[test]
    fn onboard_recommendations_are_forwarded_without_daemon_guard() {
        let unchanged: [&[&str]; 11] = [
            &["onboard", "recommendations"],
            &["onboard", "recommendations", "--json"],
            &["onboard", "--json", "recommendations"],
            &[
                "onboard",
                "recommendations",
                "--agent",
                "main",
                "acknowledge",
            ],
            &["onboard", "recommendations", "refresh"],
            &["--log-level", "debug", "onboard", "recommendations"],
            &[
                "onboard",
                "--no-color",
                "--profile",
                "work",
                "recommendations",
            ],
            &["onboard", "--log-level=debug", "--json", "recommendations"],
            &["--profile", "onboard", "onboard", "recommendations"],
            &["--", "onboard", "recommendations"],
            &["--", "onboard", "--json", "recommendations"],
        ];
        for forwarded in unchanged {
            assert_eq!(
                normalize_openclaw_args_for_env(&args(forwarded)).unwrap(),
                args(forwarded),
                "{forwarded:?}"
            );
        }

        let onboarding: [&[&str]; 5] = [
            &["onboard", "--workspace", "recommendations"],
            &["onboard", "--accept-risk", "recommendations"],
            &["onboard", "--", "recommendations"],
            &["onboard", "status", "recommendations"],
            &["--log-level", "recommendations", "onboard"],
        ];
        for forwarded in onboarding {
            let mut expected = args(forwarded);
            expected.push("--no-install-daemon".to_string());
            assert_eq!(
                normalize_openclaw_args_for_env(&args(forwarded)).unwrap(),
                expected,
                "{forwarded:?}"
            );
        }
    }
}
