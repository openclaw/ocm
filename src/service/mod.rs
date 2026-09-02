pub(crate) mod inspect;
mod manage;
pub(crate) mod platform;
mod readiness;

use std::collections::BTreeMap;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

pub use inspect::{ServiceSummary, ServiceSummaryList};
pub use manage::{ServiceActionSummary, ServiceInstallSummary, ServiceRestartOptions};
pub(crate) use platform::{
    ServiceManagerKind, service_backend_support_error, service_manager_kind,
};
pub(crate) use readiness::wait_for_gateway_readiness;

pub struct ServiceService<'a> {
    env: &'a BTreeMap<String, String>,
    cwd: &'a Path,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ServiceMaintenanceState {
    pub(crate) enabled: bool,
    pub(crate) running: bool,
}

impl<'a> ServiceService<'a> {
    pub fn new(env: &'a BTreeMap<String, String>, cwd: &'a Path) -> Self {
        Self { env, cwd }
    }

    pub fn list(&self) -> Result<ServiceSummaryList, String> {
        inspect::list_services(self.env, self.cwd)
    }

    pub fn status(&self, name: &str) -> Result<ServiceSummary, String> {
        inspect::service_status_fast(name, self.env, self.cwd)
    }

    pub fn install(&self, name: &str) -> Result<ServiceInstallSummary, String> {
        let _lock = crate::env::EnvironmentService::new(self.env, self.cwd).lock_operation(name)?;
        manage::install_service(name, self.env, self.cwd)
    }

    pub fn start(&self, name: &str) -> Result<ServiceActionSummary, String> {
        let summary = self.start_action(name)?;
        summary.ensure_gateway_ready()?;
        Ok(summary)
    }

    pub(crate) fn start_action(&self, name: &str) -> Result<ServiceActionSummary, String> {
        let _lock = crate::env::EnvironmentService::new(self.env, self.cwd).lock_operation(name)?;
        let mut summary = self.start_locked(name)?;
        self.apply_gateway_readiness(name, &mut summary)?;
        Ok(summary)
    }

    pub(crate) fn start_locked(&self, name: &str) -> Result<ServiceActionSummary, String> {
        manage::start_service(name, self.env, self.cwd)
    }

    pub fn stop(&self, name: &str) -> Result<ServiceActionSummary, String> {
        let _lock = crate::env::EnvironmentService::new(self.env, self.cwd).lock_operation(name)?;
        self.stop_locked(name)
    }

    pub(crate) fn stop_locked(&self, name: &str) -> Result<ServiceActionSummary, String> {
        manage::stop_service(name, self.env, self.cwd)
    }

    pub(crate) fn quiesce_for_runtime_mutation_locked(&self, name: &str) -> Result<bool, String> {
        let meta = crate::env::EnvironmentService::new(self.env, self.cwd).get(name)?;
        if !meta.service_enabled || !meta.service_running {
            return Ok(false);
        }
        self.stop_locked(name)?;
        if let Err(error) = self.wait_for_runtime_mutation_quiescence_locked(name) {
            return match self.start_locked(name) {
                Ok(_) => Err(error),
                Err(restore_error) => Err(format!(
                    "{error}; also failed to restore the pre-maintenance service state: {restore_error}"
                )),
            };
        }
        Ok(true)
    }

    pub(crate) fn wait_for_binding_convergence_locked(
        &self,
        name: &str,
        expected_kind: &str,
        expected_name: &str,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let meta = crate::env::EnvironmentService::new(self.env, self.cwd).get(name)?;
            let meta_matches = match expected_kind {
                "runtime" => meta.default_runtime.as_deref() == Some(expected_name),
                "launcher" => meta.default_launcher.as_deref() == Some(expected_name),
                _ => false,
            };
            let inspection =
                crate::supervisor::SupervisorService::new(self.env, self.cwd).inspect()?;
            let planned_matches = inspection
                .planned_children
                .iter()
                .filter(|child| child.env_name == name)
                .all(|child| {
                    child.binding_kind == expected_kind && child.binding_name == expected_name
                });
            let runtime_matches = inspection
                .runtime_children
                .iter()
                .filter(|child| child.env_name == name)
                .all(|child| {
                    child.binding_kind == expected_kind && child.binding_name == expected_name
                })
                && inspection
                    .runtime_services
                    .iter()
                    .filter(|service| service.env_name == name)
                    .all(|service| {
                        service.binding_kind == expected_kind
                            && service.binding_name == expected_name
                    });
            if meta_matches && planned_matches && runtime_matches {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "supervisor did not acknowledge restored {expected_kind} binding \"{expected_name}\" for env \"{name}\" before runtime cleanup"
                ));
            }
            sleep(Duration::from_millis(25));
        }
    }

    fn wait_for_runtime_mutation_quiescence_locked(&self, name: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let inspection =
                crate::supervisor::SupervisorService::new(self.env, self.cwd).inspect()?;
            let acknowledged = !inspection
                .planned_children
                .iter()
                .any(|child| child.env_name == name)
                && !inspection
                    .runtime_children
                    .iter()
                    .any(|child| child.env_name == name)
                && !inspection
                    .runtime_services
                    .iter()
                    .any(|service| service.env_name == name);
            if acknowledged {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "supervisor did not acknowledge quiescence for env \"{name}\" before runtime mutation"
                ));
            }
            sleep(Duration::from_millis(25));
        }
    }

    pub(crate) fn quiesce_for_snapshot_locked(
        &self,
        name: &str,
    ) -> Result<Option<ServiceMaintenanceState>, String> {
        let status = self.status(name)?;
        let meta = crate::env::EnvironmentService::new(self.env, self.cwd).get(name)?;
        let state = ServiceMaintenanceState {
            enabled: meta.service_enabled,
            running: meta.service_running,
        };
        if !state.running && !status.running {
            return Ok(None);
        }
        self.preserve_operation_owner_before_managed_quiescence(name, &status)?;
        if state.running {
            self.stop_locked(name)?;
        }
        if let Err(error) = self.wait_for_runtime_mutation_quiescence_locked(name) {
            return match self.restore_after_snapshot_locked(name, Some(state)) {
                Ok(()) => Err(error),
                Err(restore_error) => Err(format!(
                    "{error}; also failed to restore its pre-snapshot service policy: {restore_error}"
                )),
            };
        }
        Ok(Some(state))
    }

    #[cfg(unix)]
    fn preserve_operation_owner_before_managed_quiescence(
        &self,
        name: &str,
        status: &ServiceSummary,
    ) -> Result<(), String> {
        // SAFETY: getpgrp has no preconditions and does not mutate memory.
        let process_group = unsafe { libc::getpgrp() };
        if !operation_owner_is_in_managed_gateway_group(
            name,
            self.env.get("OCM_ACTIVE_ENV").map(String::as_str),
            status.child_pid,
            process_group,
        ) {
            return Ok(());
        }

        // SAFETY: getpid has no preconditions and does not mutate memory.
        let process_id = unsafe { libc::getpid() };
        if process_id == process_group {
            return Err(format!(
                "cannot quiesce env \"{name}\" from inside its managed Gateway process group because the OCM operation owns that group; run the command from a process outside the Gateway"
            ));
        }

        // SAFETY: setpgid(0, 0) moves only the calling process into a new group
        // whose id is its own pid. No borrowed memory crosses the syscall.
        if unsafe { libc::setpgid(0, 0) } == -1 {
            return Err(format!(
                "failed to preserve the OCM operation before quiescing env \"{name}\": could not move process {process_id} out of managed Gateway process group {process_group}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn preserve_operation_owner_before_managed_quiescence(
        &self,
        _name: &str,
        _status: &ServiceSummary,
    ) -> Result<(), String> {
        Ok(())
    }

    pub(crate) fn restore_after_snapshot_locked(
        &self,
        name: &str,
        state: Option<ServiceMaintenanceState>,
    ) -> Result<(), String> {
        if state.is_some_and(|state| state.enabled && state.running) {
            let meta = crate::env::EnvironmentService::new(self.env, self.cwd).get(name)?;
            if meta.service_enabled && meta.service_running && self.status(name)?.running {
                return Ok(());
            }
            self.start_locked(name)?;
            self.wait_for_snapshot_running_state_locked(name)?;
        }
        Ok(())
    }

    fn wait_for_snapshot_running_state_locked(&self, name: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.status(name)?.running {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "supervisor did not restore the running service state for env \"{name}\" after snapshot maintenance"
                ));
            }
            sleep(Duration::from_millis(25));
        }
    }

    pub fn restart(&self, name: &str) -> Result<ServiceActionSummary, String> {
        self.restart_with_options(name, ServiceRestartOptions::default())
    }

    pub fn restart_with_options(
        &self,
        name: &str,
        options: ServiceRestartOptions,
    ) -> Result<ServiceActionSummary, String> {
        let summary = self.restart_action_with_options(name, options)?;
        summary.ensure_gateway_ready()?;
        Ok(summary)
    }

    pub(crate) fn restart_action_with_options(
        &self,
        name: &str,
        options: ServiceRestartOptions,
    ) -> Result<ServiceActionSummary, String> {
        let _lock = crate::env::EnvironmentService::new(self.env, self.cwd).lock_operation(name)?;
        let mut summary = self.restart_locked_with_options(name, options)?;
        if !manage::restart_originates_inside_gateway(name, self.env) {
            self.apply_gateway_readiness(name, &mut summary)?;
        }
        Ok(summary)
    }

    pub(crate) fn restart_locked_with_options(
        &self,
        name: &str,
        options: ServiceRestartOptions,
    ) -> Result<ServiceActionSummary, String> {
        manage::restart_service(name, options, self.env, self.cwd)
    }

    fn apply_gateway_readiness(
        &self,
        name: &str,
        summary: &mut ServiceActionSummary,
    ) -> Result<(), String> {
        if self
            .env
            .get("OCM_INTERNAL_SKIP_SERVICE_READINESS")
            .is_some_and(|value| value == "1")
        {
            return Ok(());
        }
        let readiness = wait_for_gateway_readiness(name, self.env, self.cwd)?;
        summary.apply_gateway_readiness(readiness);
        Ok(())
    }

    pub fn uninstall(&self, name: &str) -> Result<ServiceActionSummary, String> {
        let _lock = crate::env::EnvironmentService::new(self.env, self.cwd).lock_operation(name)?;
        self.uninstall_locked(name)
    }

    pub(crate) fn uninstall_locked(&self, name: &str) -> Result<ServiceActionSummary, String> {
        manage::uninstall_service(name, self.env, self.cwd)
    }
}

#[cfg(unix)]
fn operation_owner_is_in_managed_gateway_group(
    target_env: &str,
    active_env: Option<&str>,
    managed_gateway_pid: Option<u32>,
    process_group: libc::pid_t,
) -> bool {
    active_env == Some(target_env)
        && managed_gateway_pid.and_then(|pid| libc::pid_t::try_from(pid).ok())
            == Some(process_group)
}

#[cfg(all(test, unix))]
mod tests {
    use super::operation_owner_is_in_managed_gateway_group;

    #[test]
    fn recognizes_an_operation_owned_by_its_target_gateway_group() {
        assert!(operation_owner_is_in_managed_gateway_group(
            "odin",
            Some("odin"),
            Some(4100),
            4100,
        ));
    }

    #[test]
    fn leaves_another_environment_operation_in_its_existing_group() {
        assert!(!operation_owner_is_in_managed_gateway_group(
            "nimbus",
            Some("odin"),
            Some(4100),
            4100,
        ));
    }

    #[test]
    fn leaves_an_external_operation_in_its_existing_group() {
        assert!(!operation_owner_is_in_managed_gateway_group(
            "odin",
            Some("odin"),
            Some(4100),
            5200,
        ));
    }
}
