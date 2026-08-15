use super::{RuntimeMeta, RuntimeReleaseSelectorKind, RuntimeService};
use crate::runtime::releases::{
    OpenClawRelease, RuntimeRelease, is_official_openclaw_releases_url,
    load_official_openclaw_release_selection, normalize_openclaw_channel_selector,
    official_openclaw_releases_url, select_official_openclaw_release_by_channel,
    select_official_openclaw_release_by_version,
};
use crate::store::{
    BuildLocalRuntimeOptions as StoreBuildLocalRuntimeOptions, InstallContext,
    PreparedRuntimeInstall, RuntimeReleaseDetails, get_runtime, install_runtime,
    install_runtime_from_local_openclaw_build, install_runtime_from_official_openclaw_release,
    install_runtime_from_release, install_runtime_from_url, list_runtimes,
    prepare_runtime_from_selected_official_openclaw_release, prepare_runtime_from_selected_release,
    runtime_integrity_issue,
};
use serde::Serialize;

#[derive(Clone, Debug)]
pub struct InstallRuntimeOptions {
    pub name: String,
    pub path: String,
    pub description: Option<String>,
    pub force: bool,
}

#[derive(Clone, Debug)]
pub struct InstallRuntimeFromUrlOptions {
    pub name: String,
    pub url: String,
    pub description: Option<String>,
    pub force: bool,
}

#[derive(Clone, Debug)]
pub struct InstallRuntimeFromReleaseOptions {
    pub name: String,
    pub manifest_url: String,
    pub version: Option<String>,
    pub channel: Option<String>,
    pub description: Option<String>,
    pub force: bool,
}

#[derive(Clone, Debug)]
pub struct InstallRuntimeFromOfficialReleaseOptions {
    pub name: String,
    pub version: Option<String>,
    pub channel: Option<String>,
    pub description: Option<String>,
    pub force: bool,
}

#[derive(Clone, Debug)]
pub struct BuildLocalRuntimeOptions {
    pub name: String,
    pub repo: String,
    pub description: Option<String>,
    pub force: bool,
    pub include_source_extensions: bool,
}

#[derive(Clone, Debug)]
pub struct UpdateRuntimeFromReleaseOptions {
    pub name: String,
    pub version: Option<String>,
    pub channel: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum ResolvedRuntimeUpdateRelease {
    Official(OpenClawRelease),
    Manifest(RuntimeRelease),
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedRuntimeUpdate {
    pub(crate) existing: RuntimeMeta,
    manifest_url: String,
    selector_kind: RuntimeReleaseSelectorKind,
    selector_value: String,
    release: ResolvedRuntimeUpdateRelease,
}

impl ResolvedRuntimeUpdate {
    pub(crate) fn release_version(&self) -> &str {
        match &self.release {
            ResolvedRuntimeUpdateRelease::Official(release) => &release.version,
            ResolvedRuntimeUpdateRelease::Manifest(release) => &release.version,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUpdateSummary {
    pub name: String,
    pub outcome: String,
    pub binary_path: Option<String>,
    pub source_kind: String,
    pub release_version: Option<String>,
    pub release_channel: Option<String>,
    pub issue: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUpdateBatchSummary {
    pub count: usize,
    pub updated: usize,
    pub skipped: usize,
    pub failed: usize,
    pub results: Vec<RuntimeUpdateSummary>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfficialRuntimePrepareAction {
    Installed,
    Reused,
    Updated,
}

pub(crate) struct StagedRuntimeInstall {
    prepared: PreparedRuntimeInstall,
    action: OfficialRuntimePrepareAction,
}

impl StagedRuntimeInstall {
    pub(crate) fn meta(&self) -> &RuntimeMeta {
        self.prepared.meta()
    }

    pub(crate) fn action(&self) -> OfficialRuntimePrepareAction {
        self.action
    }

    pub(crate) fn prepared_binary_path(&self) -> std::path::PathBuf {
        self.prepared.prepared_binary_path()
    }

    pub(crate) fn commit(self) -> Result<RuntimeMeta, String> {
        self.prepared.commit()
    }
}

impl<'a> RuntimeService<'a> {
    pub fn canonical_official_openclaw_runtime_name(
        version: Option<&str>,
        channel: Option<&str>,
    ) -> Result<String, String> {
        let version = version
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let channel = channel
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(normalize_openclaw_channel_selector)
            .transpose()?;

        match (version, channel) {
            (Some(version), None) => Ok(version),
            (None, Some(channel)) => Ok(channel),
            (None, None) => Err(
                "official OpenClaw runtime selection requires --version or --channel".to_string(),
            ),
            (Some(_), Some(_)) => Err(
                "official OpenClaw runtime selection accepts only one of --version or --channel"
                    .to_string(),
            ),
        }
    }

    pub fn ensure_official_openclaw_runtime(
        &self,
        version: Option<String>,
        channel: Option<String>,
    ) -> Result<RuntimeMeta, String> {
        Ok(self
            .prepare_official_openclaw_runtime(InstallRuntimeFromOfficialReleaseOptions {
                name: Self::canonical_official_openclaw_runtime_name(
                    version.as_deref(),
                    channel.as_deref(),
                )?,
                version,
                channel,
                description: None,
                force: false,
            })?
            .0)
    }

    pub fn prepare_official_openclaw_runtime(
        &self,
        options: InstallRuntimeFromOfficialReleaseOptions,
    ) -> Result<(RuntimeMeta, OfficialRuntimePrepareAction), String> {
        self.prepare_official_openclaw_runtime_with_refresh(options, true, true, None)
    }

    pub(crate) fn prepare_selected_official_openclaw_runtime_deferred(
        &self,
        options: InstallRuntimeFromOfficialReleaseOptions,
        selected_release: OpenClawRelease,
    ) -> Result<StagedRuntimeInstall, String> {
        self.prepare_official_openclaw_runtime_staged(options, false, Some(selected_release))
    }

    fn prepare_official_openclaw_runtime_with_refresh(
        &self,
        options: InstallRuntimeFromOfficialReleaseOptions,
        refresh_supervisor: bool,
        require_unbound: bool,
        selected_release: Option<OpenClawRelease>,
    ) -> Result<(RuntimeMeta, OfficialRuntimePrepareAction), String> {
        let prepared = self.prepare_official_openclaw_runtime_staged(
            options,
            require_unbound,
            selected_release,
        )?;
        let action = prepared.action();
        let meta = prepared.commit()?;
        if refresh_supervisor {
            self.refresh_supervisor_if_present()?;
        }
        Ok((meta, action))
    }

    fn prepare_official_openclaw_runtime_staged(
        &self,
        options: InstallRuntimeFromOfficialReleaseOptions,
        require_unbound: bool,
        selected_release: Option<OpenClawRelease>,
    ) -> Result<StagedRuntimeInstall, String> {
        let version = options
            .version
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let channel = options
            .channel
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let channel = channel
            .as_deref()
            .map(normalize_openclaw_channel_selector)
            .transpose()?;

        if version.is_some() && channel.is_some() {
            return Err(
                "official OpenClaw runtime selection accepts only one of --version or --channel"
                    .to_string(),
            );
        }

        let runtime_name =
            Self::canonical_official_openclaw_runtime_name(version.as_deref(), channel.as_deref())?;
        if options.name != runtime_name {
            return Err(format!(
                "official runtime installs use the canonical name \"{runtime_name}\" for this selector"
            ));
        }

        let releases_url = official_openclaw_releases_url(self.env);
        let selected_release = match selected_release {
            Some(selected_release) => {
                let matches_selector = match (version.as_deref(), channel.as_deref()) {
                    (Some(version), None) => selected_release.version == version,
                    (None, Some(channel)) => {
                        selected_release.channel.as_deref() == Some(channel)
                            || selected_release
                                .channels
                                .iter()
                                .any(|value| value == channel)
                    }
                    _ => false,
                };
                if !matches_selector {
                    return Err(format!(
                        "resolved OpenClaw release \"{}\" no longer matches the requested selector",
                        selected_release.version
                    ));
                }
                selected_release
            }
            None => {
                let releases = load_official_openclaw_release_selection(&releases_url)?;
                match (version.as_deref(), channel.as_deref()) {
                    (Some(version), None) => {
                        select_official_openclaw_release_by_version(&releases, version)?
                    }
                    (None, Some(channel)) => {
                        select_official_openclaw_release_by_channel(&releases, channel)?
                    }
                    _ => unreachable!("validated above"),
                }
            }
        };

        let mut existing_meta = None;
        if let Ok(existing) = get_runtime(&runtime_name, self.env, self.cwd) {
            if !is_official_openclaw_releases_url(existing.source_manifest_url.as_deref(), self.env)
            {
                return Err(format!(
                    "runtime \"{runtime_name}\" already exists and is not an official OpenClaw runtime; use --runtime {runtime_name} instead"
                ));
            }

            let healthy = runtime_integrity_issue(&existing, self.env).is_none();
            let same_release = existing.release_version.as_deref()
                == Some(selected_release.version.as_str())
                && existing.source_url.as_deref() == Some(selected_release.tarball_url.as_str());
            let matches_requested_selector = match (version.as_deref(), channel.as_deref()) {
                (Some(requested_version), None) => {
                    existing.release_version.as_deref() == Some(requested_version)
                        || (existing.release_selector_kind
                            == Some(RuntimeReleaseSelectorKind::Version)
                            && existing.release_selector_value.as_deref()
                                == Some(requested_version))
                }
                (None, Some(requested_channel)) => {
                    existing.release_selector_kind == Some(RuntimeReleaseSelectorKind::Channel)
                        && existing.release_selector_value.as_deref() == Some(requested_channel)
                }
                _ => false,
            };

            if !options.force && healthy && same_release && matches_requested_selector {
                return Ok(StagedRuntimeInstall {
                    prepared: PreparedRuntimeInstall::reuse(existing),
                    action: OfficialRuntimePrepareAction::Reused,
                });
            }

            existing_meta = Some(existing);
        }

        let description = options.description.or_else(|| {
            existing_meta
                .as_ref()
                .and_then(|meta| meta.description.clone())
        });
        let install = || {
            prepare_runtime_from_selected_official_openclaw_release(
                runtime_name.clone(),
                options.force || existing_meta.is_some(),
                releases_url,
                selected_release,
                RuntimeReleaseDetails::with_selector(
                    match (version.as_deref(), channel.as_deref()) {
                        (Some(_), None) => Some(RuntimeReleaseSelectorKind::Version),
                        (None, Some(_)) => Some(RuntimeReleaseSelectorKind::Channel),
                        _ => None,
                    },
                    match (version, channel) {
                        (Some(version), None) => Some(version),
                        (None, Some(channel)) => Some(channel),
                        _ => None,
                    },
                ),
                description,
                InstallContext {
                    env: self.env,
                    cwd: self.cwd,
                },
            )
        };
        let install = if require_unbound {
            self.with_unbound_runtime(&runtime_name, install)?
        } else {
            install()?
        };
        let action = if install.reused() {
            OfficialRuntimePrepareAction::Reused
        } else if existing_meta.is_some() {
            OfficialRuntimePrepareAction::Updated
        } else {
            OfficialRuntimePrepareAction::Installed
        };
        Ok(StagedRuntimeInstall {
            prepared: install,
            action,
        })
    }

    pub fn install(&self, options: InstallRuntimeOptions) -> Result<RuntimeMeta, String> {
        let name = options.name.clone();
        let meta =
            self.with_unbound_runtime(&name, || install_runtime(options, self.env, self.cwd))?;
        self.refresh_supervisor_if_present()?;
        Ok(meta)
    }

    pub fn install_from_url(
        &self,
        options: InstallRuntimeFromUrlOptions,
    ) -> Result<RuntimeMeta, String> {
        let name = options.name.clone();
        let meta = self.with_unbound_runtime(&name, || {
            install_runtime_from_url(options, self.env, self.cwd)
        })?;
        self.refresh_supervisor_if_present()?;
        Ok(meta)
    }

    pub fn install_from_release(
        &self,
        options: InstallRuntimeFromReleaseOptions,
    ) -> Result<RuntimeMeta, String> {
        let name = options.name.clone();
        let meta = self.with_unbound_runtime(&name, || {
            install_runtime_from_release(options, self.env, self.cwd)
        })?;
        self.refresh_supervisor_if_present()?;
        Ok(meta)
    }

    pub fn install_from_official_openclaw_release(
        &self,
        options: InstallRuntimeFromOfficialReleaseOptions,
    ) -> Result<RuntimeMeta, String> {
        let name = options.name.clone();
        let meta = self.with_unbound_runtime(&name, || {
            install_runtime_from_official_openclaw_release(options, self.env, self.cwd)
        })?;
        self.refresh_supervisor_if_present()?;
        Ok(meta)
    }

    pub fn build_local(&self, options: BuildLocalRuntimeOptions) -> Result<RuntimeMeta, String> {
        let name = options.name.clone();
        let meta = self.with_unbound_runtime(&name, || {
            install_runtime_from_local_openclaw_build(
                StoreBuildLocalRuntimeOptions {
                    name: options.name,
                    repo: options.repo,
                    description: options.description,
                    force: options.force,
                    include_source_extensions: options.include_source_extensions,
                },
                InstallContext {
                    env: self.env,
                    cwd: self.cwd,
                },
            )
        })?;
        self.refresh_supervisor_if_present()?;
        Ok(meta)
    }

    pub fn update_from_release(
        &self,
        options: UpdateRuntimeFromReleaseOptions,
    ) -> Result<RuntimeMeta, String> {
        let name = options.name.clone();
        self.with_unbound_runtime(&name, || {
            let resolved = self.resolve_update_from_release(options)?;
            self.apply_resolved_update(resolved, true)
        })
    }

    pub(crate) fn resolve_update_from_release(
        &self,
        options: UpdateRuntimeFromReleaseOptions,
    ) -> Result<ResolvedRuntimeUpdate, String> {
        if options.version.is_some() && options.channel.is_some() {
            return Err("runtime update accepts only one of --version or --channel".to_string());
        }

        let existing = get_runtime(&options.name, self.env, self.cwd)?;
        let manifest_url = existing.source_manifest_url.clone().ok_or_else(|| {
            format!(
                "runtime \"{}\" is not backed by a release manifest",
                existing.name
            )
        })?;
        let (version, channel) = match (options.version, options.channel) {
            (Some(version), None) => (Some(version), None),
            (None, Some(channel)) => (None, Some(channel)),
            (None, None) => match (
                existing.release_selector_kind.clone(),
                existing.release_selector_value.clone(),
            ) {
                (Some(RuntimeReleaseSelectorKind::Version), Some(value)) => (Some(value), None),
                (Some(RuntimeReleaseSelectorKind::Channel), Some(value)) => (None, Some(value)),
                _ => {
                    return Err(format!(
                        "runtime \"{}\" does not have a stored release selector; pass --version or --channel",
                        existing.name
                    ));
                }
            },
            _ => unreachable!("conflicting selectors are rejected above"),
        };
        let (selector_kind, selector_value) = match (&version, &channel) {
            (Some(version), None) => (RuntimeReleaseSelectorKind::Version, version.clone()),
            (None, Some(channel)) => (RuntimeReleaseSelectorKind::Channel, channel.clone()),
            _ => unreachable!("runtime update requires a resolved selector"),
        };

        let release = if is_official_openclaw_releases_url(Some(manifest_url.as_str()), self.env) {
            let release = self
                .official_openclaw_releases(version.as_deref(), channel.as_deref())?
                .into_iter()
                .next()
                .ok_or_else(|| "OpenClaw release was not found".to_string())?;
            ResolvedRuntimeUpdateRelease::Official(release)
        } else {
            let release = self
                .releases_from_manifest(&manifest_url, version.as_deref(), channel.as_deref())?
                .into_iter()
                .next()
                .ok_or_else(|| "runtime release was not found".to_string())?;
            ResolvedRuntimeUpdateRelease::Manifest(release)
        };

        Ok(ResolvedRuntimeUpdate {
            existing,
            manifest_url,
            selector_kind,
            selector_value,
            release,
        })
    }

    pub(crate) fn apply_resolved_update(
        &self,
        resolved: ResolvedRuntimeUpdate,
        refresh_supervisor: bool,
    ) -> Result<RuntimeMeta, String> {
        let prepared = self.prepare_resolved_update(resolved)?;
        let meta = prepared.commit()?;
        if refresh_supervisor {
            self.refresh_supervisor_if_present()?;
        }
        Ok(meta)
    }

    pub(crate) fn prepare_resolved_update(
        &self,
        resolved: ResolvedRuntimeUpdate,
    ) -> Result<StagedRuntimeInstall, String> {
        let selector_kind = Some(resolved.selector_kind);
        let selector_value = Some(resolved.selector_value);
        let prepared = match resolved.release {
            ResolvedRuntimeUpdateRelease::Official(release) => {
                prepare_runtime_from_selected_official_openclaw_release(
                    resolved.existing.name,
                    true,
                    resolved.manifest_url,
                    release,
                    RuntimeReleaseDetails::with_selector(selector_kind, selector_value),
                    resolved.existing.description,
                    InstallContext {
                        env: self.env,
                        cwd: self.cwd,
                    },
                )?
            }
            ResolvedRuntimeUpdateRelease::Manifest(release) => {
                prepare_runtime_from_selected_release(
                    resolved.existing.name,
                    true,
                    resolved.manifest_url,
                    release,
                    selector_kind,
                    selector_value,
                    resolved.existing.description,
                    self.env,
                    self.cwd,
                )?
            }
        };
        Ok(StagedRuntimeInstall {
            prepared,
            action: OfficialRuntimePrepareAction::Updated,
        })
    }

    pub fn update_all_from_release(
        &self,
        version: Option<String>,
        channel: Option<String>,
    ) -> Result<RuntimeUpdateBatchSummary, String> {
        if version.is_some() && channel.is_some() {
            return Err("runtime update accepts only one of --version or --channel".to_string());
        }

        let runtimes = list_runtimes(self.env, self.cwd)?;
        let mut out = Vec::with_capacity(runtimes.len());
        for runtime in runtimes {
            if runtime.source_manifest_url.is_none() {
                out.push(RuntimeUpdateSummary {
                    name: runtime.name,
                    outcome: "skipped".to_string(),
                    binary_path: Some(runtime.binary_path),
                    source_kind: runtime.source_kind.as_str().to_string(),
                    release_version: runtime.release_version,
                    release_channel: runtime.release_channel,
                    issue: Some("runtime is not backed by a release manifest".to_string()),
                });
                continue;
            }

            match self.update_from_release(UpdateRuntimeFromReleaseOptions {
                name: runtime.name.clone(),
                version: version.clone(),
                channel: channel.clone(),
            }) {
                Ok(meta) => out.push(RuntimeUpdateSummary {
                    name: meta.name,
                    outcome: "updated".to_string(),
                    binary_path: Some(meta.binary_path),
                    source_kind: meta.source_kind.as_str().to_string(),
                    release_version: meta.release_version,
                    release_channel: meta.release_channel,
                    issue: None,
                }),
                Err(error) => out.push(RuntimeUpdateSummary {
                    name: runtime.name,
                    outcome: "failed".to_string(),
                    binary_path: Some(runtime.binary_path),
                    source_kind: runtime.source_kind.as_str().to_string(),
                    release_version: runtime.release_version,
                    release_channel: runtime.release_channel,
                    issue: Some(error),
                }),
            }
        }
        let updated = out
            .iter()
            .filter(|summary| summary.outcome == "updated")
            .count();
        let skipped = out
            .iter()
            .filter(|summary| summary.outcome == "skipped")
            .count();
        let failed = out
            .iter()
            .filter(|summary| summary.outcome == "failed")
            .count();
        Ok(RuntimeUpdateBatchSummary {
            count: out.len(),
            updated,
            skipped,
            failed,
            results: out,
        })
    }
}
