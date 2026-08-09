pub(crate) mod inspect;
mod manage;
pub(crate) mod platform;

use std::collections::BTreeMap;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

pub use inspect::{ServiceSummary, ServiceSummaryList};
pub use manage::{ServiceActionSummary, ServiceInstallSummary};
pub(crate) use platform::{
    ServiceManagerKind, service_backend_support_error, service_manager_kind,
};

pub struct ServiceService<'a> {
    env: &'a BTreeMap<String, String>,
    cwd: &'a Path,
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
        let _lock = crate::env::EnvironmentService::new(self.env, self.cwd).lock_operation(name)?;
        self.start_locked(name)
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

    pub fn restart(&self, name: &str) -> Result<ServiceActionSummary, String> {
        let _lock = crate::env::EnvironmentService::new(self.env, self.cwd).lock_operation(name)?;
        self.restart_locked(name)
    }

    pub(crate) fn restart_locked(&self, name: &str) -> Result<ServiceActionSummary, String> {
        manage::restart_service(name, self.env, self.cwd)
    }

    pub fn uninstall(&self, name: &str) -> Result<ServiceActionSummary, String> {
        let _lock = crate::env::EnvironmentService::new(self.env, self.cwd).lock_operation(name)?;
        self.uninstall_locked(name)
    }

    pub(crate) fn uninstall_locked(&self, name: &str) -> Result<ServiceActionSummary, String> {
        manage::uninstall_service(name, self.env, self.cwd)
    }
}
