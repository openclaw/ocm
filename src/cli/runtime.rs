use crate::runtime::{
    AddRuntimeOptions, BuildLocalRuntimeOptions, InstallRuntimeFromOfficialReleaseOptions,
    InstallRuntimeFromReleaseOptions, InstallRuntimeFromUrlOptions, InstallRuntimeOptions,
    OfficialRuntimePrepareAction, RuntimeService, UpdateRuntimeFromReleaseOptions,
};

use super::{Cli, render};

impl Cli {
    pub(super) fn handle_runtime_add(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "runtime add")?;
        let (args, path) = Self::consume_option(args, "--path")?;
        let path = Self::require_option_value(path, "--path")?;
        let (args, description) = Self::consume_option(args, "--description")?;
        let Some(name) = args.first() else {
            return Err("runtime name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let meta = self.runtime_service().add(AddRuntimeOptions {
            name: name.clone(),
            path: path.unwrap_or_default(),
            description,
        })?;

        if json_flag {
            self.print_json(&meta)?;
            return Ok(0);
        }

        self.stdout_lines(render::runtime::runtime_added(
            &meta,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_runtime_list(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "runtime list")?;
        Self::assert_no_extra_args(&args)?;

        let runtimes = self.runtime_service().list()?;
        if json_flag {
            self.print_json(&runtimes)?;
            return Ok(0);
        }
        self.stdout_lines(render::runtime::runtime_list(&runtimes, profile));
        Ok(0)
    }

    pub(super) fn handle_runtime_show(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "runtime show")?;
        let Some(name) = args.first() else {
            return Err("runtime name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let meta = self.runtime_service().show(name)?;
        if json_flag {
            self.print_json(&meta)?;
            return Ok(0);
        }

        self.stdout_lines(render::runtime::runtime_show(
            &meta,
            profile,
            &self.command_example(),
        )?);
        Ok(0)
    }

    pub(super) fn handle_runtime_which(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "runtime which")?;
        let Some(name) = args.first() else {
            return Err("runtime name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary = self.runtime_service().which(name)?;
        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }
        self.stdout_lines(render::runtime::runtime_which(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_runtime_remove(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "runtime remove")?;
        let Some(name) = args.first() else {
            return Err("runtime name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let meta = self.runtime_service().remove(name)?;
        if json_flag {
            self.print_json(&meta)?;
            return Ok(0);
        }
        self.stdout_lines(render::runtime::runtime_removed(
            &meta.name,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_runtime_install(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "runtime install")?;
        let (args, force) = Self::consume_flag(args, "--force");
        let (args, path) = Self::consume_option(args, "--path")?;
        let path = Self::require_option_value(path, "--path")?;
        let (args, url) = Self::consume_option(args, "--url")?;
        let url = Self::require_option_value(url, "--url")?;
        let (args, manifest_url) = Self::consume_option(args, "--manifest-url")?;
        let manifest_url = Self::require_option_value(manifest_url, "--manifest-url")?;
        let (args, version) = Self::consume_option(args, "--version")?;
        let version = Self::require_option_value(version, "--version")?;
        let (args, channel) = Self::consume_option(args, "--channel")?;
        let channel = Self::require_option_value(channel, "--channel")?;
        let (args, description) = Self::consume_option(args, "--description")?;
        if args.len() > 1 {
            Self::assert_no_extra_args(&args[1..])?;
        }
        let explicit_name = args.first().cloned();

        let source_count = usize::from(path.is_some())
            + usize::from(url.is_some())
            + usize::from(manifest_url.is_some());
        if source_count > 1 {
            return Err(
                "runtime install accepts only one of --path, --url, or --manifest-url".to_string(),
            );
        }
        if manifest_url.is_none()
            && (path.is_some() || url.is_some())
            && (version.is_some() || channel.is_some())
        {
            return Err(
                "runtime install only supports --version or --channel with the official release source or --manifest-url".to_string(),
            );
        }

        let resolve_official_name = || -> Result<String, String> {
            let canonical = RuntimeService::canonical_official_openclaw_runtime_name(
                version.as_deref(),
                channel.as_deref(),
            )?;
            match explicit_name.as_deref() {
                Some(name) if name == canonical.as_str() => Ok(canonical),
                Some(_) => Err(format!(
                    "official runtime installs use the canonical name \"{canonical}\" for this selector"
                )),
                None => Ok(canonical),
            }
        };

        let meta = match (path, url, manifest_url) {
            (Some(path), None, None) => {
                let Some(name) = explicit_name.clone() else {
                    return Err("runtime name is required".to_string());
                };
                self.with_progress(format!("Installing runtime {name}"), || {
                    self.runtime_service().install(InstallRuntimeOptions {
                        name: name.clone(),
                        path,
                        description,
                        force,
                    })
                })?
            }
            (None, Some(url), None) => {
                let Some(name) = explicit_name.clone() else {
                    return Err("runtime name is required".to_string());
                };
                self.with_progress(format!("Downloading runtime {name}"), || {
                    self.runtime_service()
                        .install_from_url(InstallRuntimeFromUrlOptions {
                            name: name.clone(),
                            url,
                            description,
                            force,
                        })
                })?
            }
            (None, None, Some(manifest_url)) => {
                let Some(name) = explicit_name.clone() else {
                    return Err("runtime name is required".to_string());
                };
                if version.is_some() && channel.is_some() {
                    return Err(
                        "runtime install with --manifest-url accepts only one of --version or --channel"
                            .to_string(),
                    );
                }
                if version.is_none() && channel.is_none() {
                    return Err(
                        "runtime install with --manifest-url requires --version or --channel"
                            .to_string(),
                    );
                }
                self.with_progress(format!("Installing runtime {name}"), || {
                    self.runtime_service()
                        .install_from_release(InstallRuntimeFromReleaseOptions {
                            name: name.clone(),
                            manifest_url,
                            version,
                            channel,
                            description,
                            force,
                        })
                })?
            }
            (None, None, None) if version.is_some() || channel.is_some() => {
                if version.is_some() && channel.is_some() {
                    return Err(
                        "runtime install accepts only one of --version or --channel".to_string()
                    );
                }
                let name = resolve_official_name()?;
                if let Some(code) =
                    self.ensure_official_release_host_ready(Some(profile), json_flag)?
                {
                    return Ok(code);
                }
                let (meta, action) =
                    self.with_progress(format!("Installing runtime {name}"), || {
                        self.runtime_service().prepare_official_openclaw_runtime(
                            InstallRuntimeFromOfficialReleaseOptions {
                                name: name.clone(),
                                version,
                                channel,
                                description,
                                force,
                            },
                        )
                    })?;

                if json_flag {
                    self.print_json(&meta)?;
                    return Ok(0);
                }

                self.stdout_lines(match action {
                    OfficialRuntimePrepareAction::Installed => {
                        render::runtime::runtime_installed(&meta, profile, &self.command_example())
                    }
                    OfficialRuntimePrepareAction::Reused => {
                        render::runtime::runtime_reused(&meta, profile, &self.command_example())
                    }
                    OfficialRuntimePrepareAction::Updated => {
                        render::runtime::runtime_updated(&meta, profile, &self.command_example())
                    }
                });
                return Ok(0);
            }
            (None, None, None) => {
                return Err(
                    "runtime install requires --path, --url, --manifest-url, or --version/--channel"
                        .to_string(),
                );
            }
            _ => unreachable!("source_count guards conflicting runtime install sources"),
        };

        if json_flag {
            self.print_json(&meta)?;
            return Ok(0);
        }

        self.stdout_lines(render::runtime::runtime_installed(
            &meta,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_runtime_build_local(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "runtime build-local")?;
        let (args, force) = Self::consume_flag(args, "--force");
        let (args, include_source_extensions) =
            Self::consume_flag(args, "--include-source-extensions");
        let (args, target_env) = Self::consume_option(args, "--for-env")?;
        let target_env = Self::require_option_value(target_env, "--for-env")?;
        if include_source_extensions && target_env.is_some() {
            return Err(
                "runtime build-local accepts only one of --include-source-extensions or --for-env"
                    .to_string(),
            );
        }
        let (args, repo) = Self::consume_option(args, "--repo")?;
        let repo = Self::require_option_value(repo, "--repo")?;
        let Some(repo) = repo else {
            return Err("--repo is required".to_string());
        };
        let (args, description) = Self::consume_option(args, "--description")?;
        let Some(name) = args.first() else {
            return Err("runtime name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let meta = self.with_progress(format!("Building runtime {name}"), || {
            self.runtime_service()
                .build_local(BuildLocalRuntimeOptions {
                    name: name.clone(),
                    repo: repo.clone(),
                    description,
                    force,
                    include_source_extensions,
                    target_env,
                })
        })?;

        if json_flag {
            self.print_json(&meta)?;
            return Ok(0);
        }

        self.stdout_lines(render::runtime::runtime_built(
            &meta,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_runtime_releases(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, _profile) =
            self.consume_human_output_flags(args, "runtime releases")?;
        let (args, version) = Self::consume_option(args, "--version")?;
        let version = Self::require_option_value(version, "--version")?;
        let (args, channel) = Self::consume_option(args, "--channel")?;
        let channel = Self::require_option_value(channel, "--channel")?;
        let (args, manifest_url) = Self::consume_option(args, "--manifest-url")?;
        let manifest_url = Self::require_option_value(manifest_url, "--manifest-url")?;
        Self::assert_no_extra_args(&args)?;

        if version.is_some() && channel.is_some() {
            return Err("runtime releases accepts only one of --version or --channel".to_string());
        }

        let releases = if let Some(manifest_url) = manifest_url.as_deref() {
            self.runtime_service().releases_from_manifest(
                manifest_url,
                version.as_deref(),
                channel.as_deref(),
            )?
        } else {
            self.runtime_service()
                .official_openclaw_releases(version.as_deref(), channel.as_deref())?
                .into_iter()
                .map(|release| crate::runtime::RuntimeRelease {
                    version: release.version,
                    channel: release.channel,
                    url: release.tarball_url,
                    sha256: None,
                    description: None,
                })
                .collect()
        };
        if json_flag {
            self.print_json(&releases)?;
            return Ok(0);
        }
        self.stdout_lines(render::runtime::runtime_releases(&releases));
        Ok(0)
    }

    pub(super) fn handle_runtime_update(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "runtime update")?;
        let (args, all_flag) = Self::consume_flag(args, "--all");
        let (args, version) = Self::consume_option(args, "--version")?;
        let version = Self::require_option_value(version, "--version")?;
        let (args, channel) = Self::consume_option(args, "--channel")?;
        let channel = Self::require_option_value(channel, "--channel")?;
        if all_flag {
            Self::assert_no_extra_args(&args)?;
            let batch = self.with_progress("Updating runtimes", || {
                self.runtime_service()
                    .update_all_from_release(version, channel)
            })?;
            let code = if batch.failed > 0 { 1 } else { 0 };

            if json_flag {
                self.print_json(&batch)?;
                return Ok(code);
            }

            self.stdout_lines(render::runtime::runtime_update_batch(&batch, profile));
            return Ok(code);
        }
        let Some(name) = args.first() else {
            return Err("runtime name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let meta = self.with_progress(format!("Updating runtime {name}"), || {
            self.runtime_service()
                .update_from_release(UpdateRuntimeFromReleaseOptions {
                    name: name.clone(),
                    version,
                    channel,
                })
        })?;

        if json_flag {
            self.print_json(&meta)?;
            return Ok(0);
        }

        self.stdout_lines(render::runtime::runtime_updated(
            &meta,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_runtime_verify(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "runtime verify")?;
        let (args, all_flag) = Self::consume_flag(args, "--all");
        if all_flag {
            Self::assert_no_extra_args(&args)?;
            let summaries = self.runtime_service().verify_all()?;
            let code = if summaries.iter().all(|summary| summary.healthy) {
                0
            } else {
                1
            };

            if json_flag {
                self.print_json(&summaries)?;
                return Ok(code);
            }

            self.stdout_lines(render::runtime::runtime_verify_all(&summaries, profile));
            return Ok(code);
        }

        let Some(name) = args.first() else {
            return Err("runtime name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary = self.runtime_service().verify(name)?;
        let code = if summary.healthy { 0 } else { 1 };

        if json_flag {
            self.print_json(&summary)?;
            return Ok(code);
        }

        self.stdout_lines(render::runtime::runtime_verify(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(code)
    }

    pub(super) fn dispatch_runtime_command(
        &self,
        action: &str,
        rest: Vec<String>,
    ) -> Result<i32, String> {
        match action {
            "add" => self.handle_runtime_add(rest),
            "install" => self.handle_runtime_install(rest),
            "build-local" => self.handle_runtime_build_local(rest),
            "update" => self.handle_runtime_update(rest),
            "releases" => self.handle_runtime_releases(rest),
            "list" => self.handle_runtime_list(rest),
            "show" => self.handle_runtime_show(rest),
            "verify" => self.handle_runtime_verify(rest),
            "which" => self.handle_runtime_which(rest),
            "remove" | "rm" => self.handle_runtime_remove(rest),
            _ => Err(format!("unknown runtime command: {action}")),
        }
    }
}
