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
use crate::store::{display_path, get_launcher, get_runtime_verified};

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

pub fn resolve_runtime_run_dir(fallback_cwd: &Path) -> PathBuf {
    fallback_cwd.to_path_buf()
}

fn gateway_shell_program_arguments(command: &str) -> Vec<String> {
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

fn normalize_openclaw_args_for_env(args: &[String]) -> Result<Vec<String>, String> {
    if !matches!(args.first().map(String::as_str), Some("onboard")) {
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
            (None, Some(command)) => gateway_shell_program_arguments(command),
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

    pub fn resolve_run(
        &self,
        name: &str,
        runtime_override: Option<String>,
        launcher_override: Option<String>,
        args: &[String],
    ) -> Result<ResolvedExecution, String> {
        self.resolve_execution(self.touch(name)?, runtime_override, launcher_override, args)
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
            let program_args = source_watch_openclaw_program_args(&source, &args);
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
                let launcher = get_launcher(&launcher_name, self.env, self.cwd)?;
                Ok(ResolvedExecution::Launcher {
                    launcher_name,
                    command: build_launcher_command(&launcher, &args),
                    run_dir: resolve_launcher_run_dir(&launcher, self.cwd),
                    env,
                })
            }
            ExecutionBinding::Runtime(runtime_name) => {
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
                    repo_root: dev.repo_root,
                    worktree_root: dev.worktree_root,
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
    let args = source_watch_openclaw_program_args(&source, &gateway_args);

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

fn source_watch_openclaw_program_args(
    source: &SourceWatchOverride,
    openclaw_args: &[String],
) -> Vec<String> {
    let mut program_args = vec![display_path(&source.openclaw_entry_path())];
    program_args.extend(openclaw_args.iter().cloned());
    program_args
}

fn source_watch_command_label(source: &SourceWatchOverride, openclaw_args: &[String]) -> String {
    let mut parts = vec![source.command_label()];
    parts.extend(openclaw_args.iter().cloned());
    parts.join(" ")
}
