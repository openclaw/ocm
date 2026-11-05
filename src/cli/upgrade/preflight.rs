use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;

use super::{Cli, RuntimeMeta, UpgradeCommandLocks};
use crate::env::resolve_runtime_run_dir;
use crate::infra::shell::build_openclaw_env;
use crate::launcher::{launcher_package_root, read_source_json};
use crate::runtime::resolve_runtime_launch;

// Native startup gets five minutes; the capture owner retains another minute
// for native cancellation/cleanup before it settles the process itself.
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(360);
const PREFLIGHT_AUTHORITY_ENV_KEYS: &[&str] = &[
    "OPENCLAW_SUPERVISOR_MODE",
    "LAUNCH_JOB_LABEL",
    "LAUNCH_JOB_NAME",
    "XPC_SERVICE_NAME",
    "OPENCLAW_LAUNCHD_LABEL",
    "OPENCLAW_SYSTEMD_UNIT",
    "INVOCATION_ID",
    "SYSTEMD_EXEC_PID",
    "JOURNAL_STREAM",
    "OPENCLAW_WINDOWS_TASK_NAME",
    "OPENCLAW_SERVICE_MARKER",
    "OPENCLAW_SERVICE_KIND",
    "OPENCLAW_CONTROL_PLANE_UPDATE_SENTINEL_META",
    "OPENCLAW_GATEWAY_SERVICE_PID",
    "OPENCLAW_COMPATIBILITY_HOST_VERSION",
    "OPENCLAW_NO_RESPAWN",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreflightReceipt {
    protocol: u64,
    mode: String,
    status: String,
    candidate_version: Option<String>,
    coverage: String,
    phase: Option<String>,
    reason: Option<String>,
}

impl Cli {
    pub(super) fn preflight_target_readiness(
        &self,
        env_name: &str,
        target: &RuntimeMeta,
        prepared_binary: &Path,
        locks: &UpgradeCommandLocks<'_>,
    ) -> Result<(), String> {
        let generation = crate::infra::interrupt::generation();
        // Resolve entrypoint links for package discovery only. Legacy standalone
        // artifacts can still have an unpublished entrypoint during preparation.
        let entrypoint = if prepared_binary.is_symlink() {
            std::fs::canonicalize(prepared_binary)
                .map_err(|error| format!("could not inspect candidate package: {error}"))?
        } else {
            prepared_binary.to_path_buf()
        };
        let package_root = entrypoint
            .parent()
            .ok_or("candidate entrypoint has no package directory")?;
        if !package_root.join("package.json").exists() {
            return Ok(());
        }
        let package_root = std::fs::canonicalize(package_root)
            .map_err(|error| format!("could not inspect candidate package: {error}"))?;
        let package = read_source_json(&package_root, "package.json", 1024 * 1024)?;
        let Some(protocol) = package.pointer("/openclaw/updatePreflightProtocol") else {
            // Legacy packages retain checkpointed cold finalization and rollback.
            return Ok(());
        };
        if protocol.as_u64() != Some(1) {
            return Err("candidate advertises an unsupported update preflight protocol; readiness is unverified".to_string());
        }
        if package.get("name").and_then(serde_json::Value::as_str) != Some("openclaw") {
            return Err(
                "update preflight metadata does not identify an OpenClaw package".to_string(),
            );
        }
        let version = package
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or("candidate package has no version for update preflight")?;
        if target
            .release_version
            .as_deref()
            .is_some_and(|expected| expected != version)
        {
            return Err("candidate package version does not match the selected runtime release; readiness is unverified".to_string());
        }
        let environment = self.environment_service().get(env_name)?;
        let source_root = if let Some(runtime) = environment.default_runtime.as_deref() {
            let source = self.runtime_service().show(runtime)?;
            let binary = std::fs::canonicalize(&source.binary_path)
                .map_err(|error| format!("could not inspect source package: {error}"))?;
            binary
                .parent()
                .ok_or("source entrypoint has no package directory")?
                .to_path_buf()
        } else if let Some(launcher) = environment.default_launcher.as_deref() {
            let source = self.launcher_service().show(launcher)?;
            launcher_package_root(&source).ok_or(
                "source launcher does not identify an OpenClaw package; readiness is unverified",
            )?
        } else {
            return Err("source binding is missing; readiness is unverified".to_string());
        };
        validate_source_package(&source_root)?;
        let args = vec![
            "update".to_string(),
            "preflight".to_string(),
            "--json".to_string(),
            "--source-root".to_string(),
            source_root.to_string_lossy().into_owned(),
            "--timeout".to_string(),
            "300".to_string(),
        ];
        // The target is still unpublished. Resolve its staged entrypoint directly,
        // with the source profile's paths and without cold-finalizer authority.
        let mut candidate = target.clone();
        candidate.binary_path = prepared_binary.to_string_lossy().into_owned();
        let launch = resolve_runtime_launch(&candidate, &args, &self.env, &self.cwd, true)?;
        let mut process_env = build_openclaw_env(&environment, &self.env);
        // Match native admission's authority boundary, including Windows'
        // case-insensitive environment and source profile supervision hints.
        process_env.retain(|key, _| {
            let key = key.to_ascii_uppercase();
            !key.starts_with("OPENCLAW_UPDATE_")
                && !PREFLIGHT_AUTHORITY_ENV_KEYS.contains(&key.as_str())
        });
        process_env.remove("NODE_COMPILE_CACHE");
        process_env.insert("NODE_DISABLE_COMPILE_CACHE".to_string(), "1".to_string());
        crate::managed_node::apply_path_prepend_to_environment(
            &mut process_env,
            launch.path_prepend.as_deref(),
        )?;
        let mut command = Command::new(&launch.program);
        command
            .args(&launch.args)
            .current_dir(resolve_runtime_run_dir(&self.cwd))
            .env_clear()
            .envs(process_env)
            .stdin(Stdio::null());
        locks.retain_for_child(&mut command);
        let (status, stdout) = crate::supervisor::run_upgrade_preflight_command(
            command,
            PREFLIGHT_TIMEOUT,
            generation,
        )?;
        let receipt: PreflightReceipt = serde_json::from_slice(&stdout)
            .map_err(|_| "candidate returned an invalid update preflight receipt".to_string())?;
        if receipt.protocol != 1
            || receipt.mode != "preflight"
            || receipt.coverage != "isolated-core-startup"
        {
            return Err("candidate update preflight receipt does not match its package and supported core-startup protocol".to_string());
        }
        if !status.success() || receipt.status != "ready" {
            let reason = receipt
                .reason
                .as_deref()
                .unwrap_or("candidate did not confirm isolated core startup");
            let detail = reason.split_whitespace().collect::<Vec<_>>().join(" ");
            let detail = detail.chars().take(512).collect::<String>();
            return Err(format!(
                "candidate update preflight {} during {}: {detail}",
                receipt.status,
                receipt.phase.as_deref().unwrap_or("execution"),
            ));
        }
        if receipt.candidate_version.as_deref() != Some(version) {
            return Err(
                "candidate update preflight receipt does not match its package version".to_string(),
            );
        }
        Ok(())
    }
}

fn validate_source_package(root: &Path) -> Result<(), String> {
    let package = read_source_json(root, "package.json", 1024 * 1024)
        .map_err(|error| format!("could not inspect source package: {error}"))?;
    if package.get("name").and_then(serde_json::Value::as_str) != Some("openclaw") {
        return Err(
            "source binding does not identify an OpenClaw package; readiness is unverified"
                .to_string(),
        );
    }
    Ok(())
}
