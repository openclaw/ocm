use std::path::Path;

use serde::Serialize;

use super::{EnvDevMeta, EnvMeta, EnvironmentService};
use crate::store::{
    get_environment, save_environment, save_environment_with_validated_launcher,
    save_environment_with_validated_runtime, save_environment_with_validated_runtime_guard,
};
use crate::supervisor::sync_supervisor_env_if_present;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevRebindSummary {
    pub env_name: String,
    pub previous_source_root: String,
    pub source_root: String,
    pub changed: bool,
}

impl<'a> EnvironmentService<'a> {
    pub(crate) fn rebind_dev(&self, name: &str, source: &Path) -> Result<DevRebindSummary, String> {
        let _operation = crate::store::try_lock_environment_operation(name, self.env, self.cwd)?
            .ok_or_else(|| {
                format!("environment {name} has an operation in progress; retry after it finishes")
            })?;
        let meta = self.get(name)?;
        let previous = meta.dev.as_ref().and_then(EnvDevMeta::borrowed_source_root)
            .filter(|_| meta.default_runtime.is_none() && meta.default_launcher.is_none())
            .ok_or("dev rebind requires an existing borrowed dev environment without a runtime or launcher binding")?;
        let source = std::fs::canonicalize(source).map_err(|error| {
            format!(
                "failed to resolve selected OpenClaw checkout {}: {error}",
                source.display()
            )
        })?;
        let dev = EnvDevMeta::Borrowed {
            source_root: crate::store::display_path(&source),
        };
        // Register only the new source: the old checkout may no longer exist.
        let registration = crate::store::DevSourceRegistration::lock_source(
            &dev,
            &self.list()?,
            self.env,
            self.cwd,
        )?;
        let _admission = self
            .try_lock_gateway_admission(name)?
            .ok_or("Gateway admission is busy; stop the dev session or service before rebinding")?;
        self.ensure_source_watch_allows_state_mutation_locked(name)?;
        if meta.service_running {
            return Err(format!(
                "stop the background service with `ocm service stop {name}` before rebinding; service policy was preserved"
            ));
        }
        crate::supervisor::SupervisorService::new(self.env, self.cwd)
            .ensure_dev_rebind_quiescent(name)?;
        let (gateway_port, _) = self.resolve_effective_gateway_port(&meta)?;
        if !crate::store::openclaw_port_family_available(gateway_port) {
            return Err("the environment's Gateway port family is in use; verify its processes have stopped before rebinding".to_string());
        }
        let summary = DevRebindSummary {
            env_name: meta.name.clone(),
            previous_source_root: previous.to_string(),
            source_root: crate::store::display_path(&source),
            changed: meta.dev.as_ref() != Some(&dev),
        };
        crate::store::rebind_environment_dev(&meta, dev, &registration, self.env, self.cwd)?;
        Ok(summary)
    }

    pub fn set_upgrade_independent_paths(
        &self,
        name: &str,
        paths: Vec<std::path::PathBuf>,
    ) -> Result<EnvMeta, String> {
        let _lock = self.lock_operation(name)?;
        let mut meta = get_environment(name, self.env, self.cwd)?;
        crate::store::validate_upgrade_independent_paths(&meta, &paths, self.env)?;
        meta.upgrade_independent_paths = paths;
        save_environment(meta, self.env, self.cwd)
    }

    pub fn set_launcher(&self, name: &str, launcher_name: &str) -> Result<EnvMeta, String> {
        let _lock = self.lock_operation(name)?;
        let mut meta = get_environment(name, self.env, self.cwd)?;
        if launcher_name.eq_ignore_ascii_case("none") {
            meta.default_launcher = None;
        } else {
            meta.default_launcher = Some(launcher_name.to_string());
            meta.default_runtime = None;
        }
        let meta = save_environment_with_validated_launcher(meta, self.env, self.cwd)?;
        sync_supervisor_env_if_present(self.env, self.cwd, name)?;
        Ok(meta)
    }

    pub fn set_runtime(&self, name: &str, runtime_name: &str) -> Result<EnvMeta, String> {
        let _lock = self.lock_operation(name)?;
        self.set_runtime_locked(name, runtime_name)
    }

    pub(crate) fn set_runtime_locked(
        &self,
        name: &str,
        runtime_name: &str,
    ) -> Result<EnvMeta, String> {
        self.set_runtime_with_guard(name, runtime_name, None)
    }

    pub(crate) fn set_runtime_with_guard(
        &self,
        name: &str,
        runtime_name: &str,
        runtime_guard: Option<&crate::store::RuntimeMutationGuard>,
    ) -> Result<EnvMeta, String> {
        let mut meta = get_environment(name, self.env, self.cwd)?;
        if runtime_name.eq_ignore_ascii_case("none") {
            meta.default_runtime = None;
        } else {
            meta.default_runtime = Some(runtime_name.to_string());
            meta.default_launcher = None;
        }
        let meta = if meta.default_runtime.is_some() {
            match runtime_guard {
                Some(guard) => save_environment_with_validated_runtime_guard(
                    meta,
                    Some(guard),
                    self.env,
                    self.cwd,
                )?,
                None => save_environment_with_validated_runtime(meta, self.env, self.cwd)?,
            }
        } else {
            save_environment(meta, self.env, self.cwd)?
        };
        sync_supervisor_env_if_present(self.env, self.cwd, name)?;
        Ok(meta)
    }
}
