use super::{RuntimeCompanionMeta, RuntimeMeta, RuntimeReleaseSelectorKind, RuntimeService};
use crate::store::{get_runtime, get_runtime_verified, list_runtimes, runtime_integrity_issue};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeVerifySummary {
    pub name: String,
    pub binary_path: String,
    pub source_kind: String,
    pub source_path: Option<String>,
    pub source_url: Option<String>,
    pub source_manifest_url: Option<String>,
    pub source_sha256: Option<String>,
    pub source_integrity: Option<String>,
    pub runtime_sha256: Option<String>,
    pub release_version: Option<String>,
    pub release_channel: Option<String>,
    pub release_selector_kind: Option<RuntimeReleaseSelectorKind>,
    pub release_selector_value: Option<String>,
    pub install_root: Option<String>,
    pub companions: Vec<RuntimeCompanionMeta>,
    pub healthy: bool,
    pub issue: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeBinarySummary {
    pub name: String,
    pub binary_path: String,
    pub source_kind: String,
    pub release_version: Option<String>,
    pub release_channel: Option<String>,
    pub release_selector_kind: Option<RuntimeReleaseSelectorKind>,
    pub release_selector_value: Option<String>,
    pub install_root: Option<String>,
}

impl<'a> RuntimeService<'a> {
    pub fn verify(&self, name: &str) -> Result<RuntimeVerifySummary, String> {
        let meta = get_runtime(name, self.env, self.cwd)?;
        Ok(build_verify_summary(meta, self.env))
    }

    pub fn verify_all(&self) -> Result<Vec<RuntimeVerifySummary>, String> {
        let runtimes = list_runtimes(self.env, self.cwd)?;
        Ok(runtimes
            .into_iter()
            .map(|meta| build_verify_summary(meta, self.env))
            .collect())
    }

    pub fn which(&self, name: &str) -> Result<RuntimeBinarySummary, String> {
        let meta = get_runtime_verified(name, self.env, self.cwd)?;
        Ok(RuntimeBinarySummary {
            name: meta.name,
            binary_path: meta.binary_path,
            source_kind: meta.source_kind.as_str().to_string(),
            release_version: meta.release_version,
            release_channel: meta.release_channel,
            release_selector_kind: meta.release_selector_kind,
            release_selector_value: meta.release_selector_value,
            install_root: meta.install_root,
        })
    }
}

fn build_verify_summary(
    meta: RuntimeMeta,
    env: &std::collections::BTreeMap<String, String>,
) -> RuntimeVerifySummary {
    let issue = runtime_integrity_issue(&meta, env);
    RuntimeVerifySummary {
        name: meta.name,
        binary_path: meta.binary_path,
        source_kind: meta.source_kind.as_str().to_string(),
        source_path: meta.source_path,
        source_url: meta.source_url,
        source_manifest_url: meta.source_manifest_url,
        source_sha256: meta.source_sha256,
        source_integrity: meta.source_integrity,
        runtime_sha256: meta.runtime_sha256,
        release_version: meta.release_version,
        release_channel: meta.release_channel,
        release_selector_kind: meta.release_selector_kind,
        release_selector_value: meta.release_selector_value,
        install_root: meta.install_root,
        companions: meta.companions,
        healthy: issue.is_none(),
        issue,
    }
}
