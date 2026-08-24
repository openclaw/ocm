use serde::{Deserialize, Serialize};
use time::Duration;
use time::OffsetDateTime;

use super::EnvironmentService;
use crate::runtime::RuntimeService;
use crate::store::{
    EnvironmentOperationLock, clone_environment, clone_environment_for_simulation,
    create_environment_with_validated_runtime, export_environment, get_environment,
    get_runtime_verified, import_environment, list_environments, lock_environment_operation,
    now_utc, remove_environment, resolve_config_gateway_port, resolve_effective_gateway_ports,
    resolve_env_gateway_port, save_environment, set_environment_service_policy,
};
use crate::supervisor::{sync_supervisor_env_if_present, sync_supervisor_if_present};

pub fn default_service_enabled() -> bool {
    true
}

pub fn default_service_running() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvDevMeta {
    pub repo_root: String,
    pub worktree_root: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvMeta {
    pub kind: String,
    pub name: String,
    pub root: String,
    pub gateway_port: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub gateway_port_auto_assigned: bool,
    #[serde(default = "default_service_enabled")]
    pub service_enabled: bool,
    #[serde(default = "default_service_running")]
    pub service_running: bool,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev: Option<EnvDevMeta>,
    pub protected: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvSummary {
    pub name: String,
    pub root: String,
    pub openclaw_home: String,
    pub state_dir: String,
    pub config_path: String,
    pub workspace_dir: String,
    pub gateway_port: Option<u32>,
    pub service_enabled: bool,
    pub service_running: bool,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub dev_repo_root: Option<String>,
    pub dev_worktree_root: Option<String>,
    pub protected: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug)]
pub struct CreateEnvironmentOptions {
    pub name: String,
    pub root: Option<String>,
    pub gateway_port: Option<u32>,
    pub service_enabled: bool,
    pub service_running: bool,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub dev: Option<EnvDevMeta>,
    pub protected: bool,
}

#[derive(Clone, Debug)]
pub struct CloneEnvironmentOptions {
    pub source_name: String,
    pub name: String,
    pub root: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CloneEnvironmentResult {
    pub meta: EnvMeta,
    pub cleared_sandbox_origin: bool,
    pub sandbox_port: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct ExportEnvironmentOptions {
    pub name: String,
    pub output: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvExportSummary {
    pub name: String,
    pub root: String,
    pub archive_path: String,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub protected: bool,
}

#[derive(Clone, Debug)]
pub struct ImportEnvironmentOptions {
    pub archive: String,
    pub name: Option<String>,
    pub root: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvImportSummary {
    pub name: String,
    pub source_name: String,
    pub root: String,
    pub archive_path: String,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub protected: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ImportEnvironmentResult {
    pub summary: EnvImportSummary,
    pub cleared_sandbox_origin: bool,
    pub sandbox_port: Option<u32>,
}

pub fn select_prune_candidates(envs: &[EnvMeta], older_than_days: i64) -> Vec<EnvMeta> {
    let cutoff = now_utc() - Duration::days(older_than_days);
    envs.iter()
        .filter(|meta| !meta.protected)
        .filter(|meta| meta.last_used_at.unwrap_or(meta.created_at) < cutoff)
        .cloned()
        .collect()
}

impl<'a> EnvironmentService<'a> {
    pub fn resolve_runtime_binding_request(
        &self,
        runtime_name: Option<String>,
        version: Option<String>,
        channel: Option<String>,
        command: &str,
    ) -> Result<Option<String>, String> {
        if runtime_name.is_some() && (version.is_some() || channel.is_some()) {
            return Err(format!(
                "{command} accepts only one runtime source: --runtime, --version, or --channel"
            ));
        }
        if version.is_some() && channel.is_some() {
            return Err(format!(
                "{command} accepts only one of --version or --channel"
            ));
        }

        if let Some(runtime_name) = runtime_name {
            get_runtime_verified(&runtime_name, self.env, self.cwd)?;
            return Ok(Some(runtime_name));
        }

        match (version, channel) {
            (Some(version), None) => Ok(Some(
                RuntimeService::new(self.env, self.cwd)
                    .ensure_official_openclaw_runtime(Some(version), None)?
                    .name,
            )),
            (None, Some(channel)) => Ok(Some(
                RuntimeService::new(self.env, self.cwd)
                    .ensure_official_openclaw_runtime(None, Some(channel))?
                    .name,
            )),
            (None, None) => Ok(None),
            _ => unreachable!("conflicting selectors are rejected above"),
        }
    }

    pub fn apply_effective_gateway_port(&self, mut meta: EnvMeta) -> Result<EnvMeta, String> {
        let (gateway_port, source) = self.resolve_effective_gateway_port(&meta)?;
        meta.gateway_port = Some(gateway_port);
        meta.gateway_port_auto_assigned = source == "computed";
        Ok(meta)
    }

    pub(crate) fn apply_effective_gateway_ports(
        &self,
        envs: Vec<EnvMeta>,
    ) -> Result<Vec<EnvMeta>, String> {
        let effective_ports = resolve_effective_gateway_ports(&envs, self.env);

        envs.into_iter()
            .map(|mut meta| {
                if let Some(port) = resolve_env_gateway_port(&meta) {
                    meta.gateway_port = Some(port);
                } else if let Some(port) = effective_ports.get(&meta.name).copied() {
                    meta.gateway_port = Some(port);
                    meta.gateway_port_auto_assigned = true;
                }
                Ok(meta)
            })
            .collect()
    }

    pub fn create(&self, options: CreateEnvironmentOptions) -> Result<EnvMeta, String> {
        let meta = create_environment_with_validated_runtime(options, self.env, self.cwd)?;
        sync_supervisor_env_if_present(self.env, self.cwd, &meta.name)?;
        Ok(meta)
    }

    pub fn clone(&self, options: CloneEnvironmentOptions) -> Result<EnvMeta, String> {
        let meta = clone_environment(options, self.env, self.cwd)?;
        sync_supervisor_env_if_present(self.env, self.cwd, &meta.name)?;
        Ok(meta)
    }

    pub(crate) fn clone_for_simulation(
        &self,
        options: CloneEnvironmentOptions,
    ) -> Result<EnvMeta, String> {
        let meta = clone_environment_for_simulation(options, self.env, self.cwd)?;
        sync_supervisor_env_if_present(self.env, self.cwd, &meta.name)?;
        Ok(meta)
    }

    pub(crate) fn clone_with_sandbox_origin(
        &self,
        options: CloneEnvironmentOptions,
        sandbox_origin: Option<String>,
    ) -> Result<CloneEnvironmentResult, String> {
        let result = crate::store::clone_environment_with_sandbox_origin(
            options,
            sandbox_origin.as_deref(),
            self.env,
            self.cwd,
        )?;
        sync_supervisor_env_if_present(self.env, self.cwd, &result.meta.name)?;
        Ok(result)
    }

    pub fn export(&self, options: ExportEnvironmentOptions) -> Result<EnvExportSummary, String> {
        export_environment(options, self.env, self.cwd)
    }

    pub fn import(&self, options: ImportEnvironmentOptions) -> Result<EnvImportSummary, String> {
        let summary = import_environment(options, self.env, self.cwd)?;
        sync_supervisor_env_if_present(self.env, self.cwd, &summary.name)?;
        Ok(summary)
    }

    pub(crate) fn import_with_sandbox_origin(
        &self,
        options: ImportEnvironmentOptions,
        sandbox_origin: Option<String>,
    ) -> Result<ImportEnvironmentResult, String> {
        let result = crate::store::import_environment_with_sandbox_origin(
            options,
            sandbox_origin.as_deref(),
            self.env,
            self.cwd,
        )?;
        sync_supervisor_env_if_present(self.env, self.cwd, &result.summary.name)?;
        Ok(result)
    }

    pub fn list(&self) -> Result<Vec<EnvMeta>, String> {
        list_environments(self.env, self.cwd)
    }

    pub fn get(&self, name: &str) -> Result<EnvMeta, String> {
        get_environment(name, self.env, self.cwd)
    }

    pub fn find(&self, name: &str) -> Result<Option<EnvMeta>, String> {
        match get_environment(name, self.env, self.cwd) {
            Ok(meta) => Ok(Some(meta)),
            Err(error) if error.contains("does not exist") => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn touch(&self, name: &str) -> Result<EnvMeta, String> {
        let _lock = self.lock_operation(name)?;
        let mut meta = get_environment(name, self.env, self.cwd)?;
        meta.last_used_at = Some(now_utc());
        save_environment(meta, self.env, self.cwd)
    }

    pub(crate) fn lock_operation(&self, name: &str) -> Result<EnvironmentOperationLock, String> {
        lock_environment_operation(name, self.env, self.cwd)
    }

    pub fn set_protected(&self, name: &str, protected: bool) -> Result<EnvMeta, String> {
        let _lock = self.lock_operation(name)?;
        let mut meta = get_environment(name, self.env, self.cwd)?;
        meta.protected = protected;
        save_environment(meta, self.env, self.cwd)
    }

    pub fn set_service_enabled(
        &self,
        name: &str,
        service_enabled: bool,
    ) -> Result<EnvMeta, String> {
        self.set_service_policy(name, Some(service_enabled), None)
    }

    pub fn set_service_running(
        &self,
        name: &str,
        service_running: bool,
    ) -> Result<EnvMeta, String> {
        self.set_service_policy(name, None, Some(service_running))
    }

    pub fn set_service_policy(
        &self,
        name: &str,
        service_enabled: Option<bool>,
        service_running: Option<bool>,
    ) -> Result<EnvMeta, String> {
        let _lock = self.lock_operation(name)?;
        let change = set_environment_service_policy(
            name,
            service_enabled,
            service_running,
            self.env,
            self.cwd,
        )?;
        sync_supervisor_if_present(self.env, self.cwd)?;
        Ok(change.applied)
    }

    pub fn remove(&self, name: &str, force: bool) -> Result<EnvMeta, String> {
        let _lock = self.lock_operation(name)?;
        self.remove_locked(name, force)
    }

    pub(crate) fn remove_locked(&self, name: &str, force: bool) -> Result<EnvMeta, String> {
        let meta = remove_environment(name, force, self.env, self.cwd)?;
        sync_supervisor_env_if_present(self.env, self.cwd, name)?;
        Ok(meta)
    }

    pub fn prune_candidates(&self, older_than_days: i64) -> Result<Vec<EnvMeta>, String> {
        let envs = list_environments(self.env, self.cwd)?;
        Ok(select_prune_candidates(&envs, older_than_days))
    }

    pub fn prune(&self, older_than_days: i64) -> Result<Vec<EnvMeta>, String> {
        let candidates = self.prune_candidates(older_than_days)?;
        let mut removed = Vec::with_capacity(candidates.len());
        for meta in candidates {
            let _lock = self.lock_operation(&meta.name)?;
            removed.push(self.remove_locked(&meta.name, false)?);
        }
        Ok(removed)
    }

    pub(crate) fn resolve_effective_gateway_port(
        &self,
        target: &EnvMeta,
    ) -> Result<(u32, &'static str), String> {
        if let Some(port) = target
            .gateway_port
            .filter(|_| !target.gateway_port_auto_assigned)
        {
            return Ok((port, "metadata"));
        }

        if let Some(port) = resolve_config_gateway_port(target) {
            return Ok((port, "config"));
        }

        if let Some(port) = target.gateway_port {
            return Ok((port, "computed"));
        }

        let effective_ports =
            resolve_effective_gateway_ports(&list_environments(self.env, self.cwd)?, self.env);

        effective_ports
            .get(&target.name)
            .copied()
            .map(|port| (port, "computed"))
            .ok_or_else(|| format!("failed to resolve gateway port for env \"{}\"", target.name))
    }
}
