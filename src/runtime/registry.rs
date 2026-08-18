use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::store::{
    add_runtime, get_runtime_verified, list_runtimes, remove_runtime, with_locked_environments,
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
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        with_locked_environments(self.env, self.cwd, |envs| {
            let bound_envs = envs
                .iter()
                .filter(|environment| environment.default_runtime.as_deref() == Some(name))
                .map(|environment| format!("\"{}\"", environment.name))
                .collect::<Vec<_>>();
            if !bound_envs.is_empty() {
                return Err(format!(
                    "runtime \"{name}\" is still used by environment(s): {}; direct runtime changes bypass per-environment snapshots and OpenClaw migration. Upgrade those environments with `ocm upgrade`, or clear their runtime bindings before changing it",
                    bound_envs.join(", ")
                ));
            }
            action()
        })
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
        let meta = self.with_unbound_runtime(&name, || add_runtime(options, self.env, self.cwd))?;
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
        let meta = self.with_unbound_runtime(name, || remove_runtime(name, self.env, self.cwd))?;
        self.refresh_supervisor_if_present(name)?;
        Ok(meta)
    }
}
