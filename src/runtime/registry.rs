use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::env::EnvMeta;
use crate::store::{
    InstallContext, RuntimeMutationGuard, add_runtime_with_context, get_runtime,
    get_runtime_verified, list_runtimes, remove_runtime_with_guard, runtime_binding_dependencies,
    runtime_meta_path, runtime_owner_identity, validate_name, with_locked_environments,
};
use crate::supervisor::sync_supervisor_binding_if_present;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeSourceKind {
    Registered,
    Installed,
}

impl RuntimeSourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Installed => "installed",
        }
    }
}

fn default_runtime_source_kind() -> RuntimeSourceKind {
    RuntimeSourceKind::Registered
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeReleaseSelectorKind {
    Version,
    Channel,
}

impl RuntimeReleaseSelectorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Channel => "channel",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCompanionMeta {
    pub id: String,
    pub package_name: String,
    pub version: String,
    pub artifact_sha256: String,
    pub entrypoint: String,
    pub entrypoint_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeMeta {
    pub kind: String,
    pub name: String,
    pub binary_path: String,
    #[serde(default = "default_runtime_source_kind")]
    pub source_kind: RuntimeSourceKind,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub source_manifest_url: Option<String>,
    #[serde(default)]
    pub source_sha256: Option<String>,
    #[serde(default)]
    pub source_integrity: Option<String>,
    #[serde(default)]
    pub runtime_sha256: Option<String>,
    #[serde(default)]
    pub release_version: Option<String>,
    #[serde(default)]
    pub release_channel: Option<String>,
    #[serde(default)]
    pub release_selector_kind: Option<RuntimeReleaseSelectorKind>,
    #[serde(default)]
    pub release_selector_value: Option<String>,
    #[serde(default)]
    pub install_root: Option<String>,
    #[serde(default)]
    pub companions: Vec<RuntimeCompanionMeta>,
    pub description: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug)]
pub struct AddRuntimeOptions {
    pub name: String,
    pub path: String,
    pub description: Option<String>,
}

pub struct RuntimeService<'a> {
    pub(super) env: &'a BTreeMap<String, String>,
    pub(super) cwd: &'a Path,
}

impl<'a> RuntimeService<'a> {
    pub(super) fn with_unbound_runtime<T>(
        &self,
        name: &str,
        action: impl FnOnce(&Arc<RuntimeMutationGuard>) -> Result<T, String>,
    ) -> Result<T, String> {
        // Wait outside the registry: an upgrade may need it to publish its
        // binding or finish recovery before releasing runtime ownership.
        let runtime_guard = Arc::new(crate::store::lock_runtime_mutation(
            name, self.env, self.cwd,
        )?);
        self.with_unbound_runtime_guard(name, &runtime_guard, action)
    }

    pub(super) fn with_unbound_runtime_guard<T>(
        &self,
        name: &str,
        runtime_guard: &Arc<RuntimeMutationGuard>,
        action: impl FnOnce(&Arc<RuntimeMutationGuard>) -> Result<T, String>,
    ) -> Result<T, String> {
        runtime_guard.check(name, self.env, self.cwd)?;
        with_locked_environments(self.env, self.cwd, |envs| {
            let bound_envs = self
                .bound_environment_names(name, envs)?
                .into_iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<_>>();
            if !bound_envs.is_empty() {
                return Err(format!(
                    "runtime \"{name}\" is still used by environment(s): {}; direct runtime changes bypass per-environment snapshots and OpenClaw migration. Upgrade those environments with `ocm upgrade`, or clear their runtime bindings before changing it",
                    bound_envs.join(", ")
                ));
            }
            action(runtime_guard)
        })
    }

    pub(crate) fn bound_environment_names(
        &self,
        name: &str,
        environments: &[EnvMeta],
    ) -> Result<Vec<String>, String> {
        let name = validate_name(name, "Runtime name")?;
        let mut dependencies = BTreeMap::new();
        let mut mutation_identity = None;
        let mut users = Vec::new();
        for environment in environments {
            let Some(selected) = environment.default_runtime.as_deref() else {
                continue;
            };
            let selected = validate_name(selected, "Runtime name")?;
            let uses_runtime = if selected == name {
                true
            } else if let Some(uses_runtime) = dependencies.get(&selected) {
                *uses_runtime
            } else {
                let registration = runtime_meta_path(&selected, self.env, self.cwd)?;
                let owners = match std::fs::symlink_metadata(registration) {
                    // Raw archive bindings may name an absent registration.
                    // There is no known indirect dependency to inspect yet.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        BTreeSet::from([selected.clone()])
                    }
                    Err(error) => return Err(error.to_string()),
                    Ok(_) => {
                        runtime_binding_dependencies(
                            &get_runtime(&selected, self.env, self.cwd)?,
                            self.env,
                            self.cwd,
                        )?
                        .owners
                    }
                };
                let mut uses_runtime = owners.contains(&name);
                if !uses_runtime {
                    let identity = match mutation_identity {
                        Some(identity) => identity,
                        None => {
                            let identity = runtime_owner_identity(&name, self.env, self.cwd)?;
                            mutation_identity = Some(identity);
                            identity
                        }
                    };
                    if let Some(identity) = identity {
                        for owner in &owners {
                            if runtime_owner_identity(owner, self.env, self.cwd)? == Some(identity)
                            {
                                uses_runtime = true;
                                break;
                            }
                        }
                    }
                }
                dependencies.insert(selected, uses_runtime);
                uses_runtime
            };
            if uses_runtime {
                users.push(environment.name.clone());
            }
        }
        Ok(users)
    }

    pub(crate) fn refresh_supervisor_if_present(&self, name: &str) -> Result<(), String> {
        sync_supervisor_binding_if_present(self.env, self.cwd, "runtime", name)?;
        Ok(())
    }

    pub fn new(env: &'a BTreeMap<String, String>, cwd: &'a Path) -> Self {
        Self { env, cwd }
    }

    pub fn add(&self, options: AddRuntimeOptions) -> Result<RuntimeMeta, String> {
        let name = options.name.clone();
        let meta = self.with_unbound_runtime(&name, |guard| {
            add_runtime_with_context(
                options,
                InstallContext {
                    env: self.env,
                    cwd: self.cwd,
                    runtime_guard: Some(guard),
                },
            )
        })?;
        self.refresh_supervisor_if_present(&name)?;
        Ok(meta)
    }

    pub fn list(&self) -> Result<Vec<RuntimeMeta>, String> {
        list_runtimes(self.env, self.cwd)
    }

    pub fn show(&self, name: &str) -> Result<RuntimeMeta, String> {
        get_runtime_verified(name, self.env, self.cwd)
    }

    pub fn remove(&self, name: &str) -> Result<RuntimeMeta, String> {
        let meta = self.with_unbound_runtime(name, |guard| {
            remove_runtime_with_guard(name, guard, self.env, self.cwd)
        })?;
        self.refresh_supervisor_if_present(name)?;
        Ok(meta)
    }
}
