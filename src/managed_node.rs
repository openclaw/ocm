use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::infra::archive::{extract_tar_gz, extract_zip};
use crate::infra::download::{download_to_file, normalize_sha256, verify_file_sha256};
use crate::store::{display_path, lock_file, resolve_store_paths};

pub const OPENCLAW_MIN_NODE_VERSION: &str = "22.22.3";
pub const OPENCLAW_NODE_VERSION_REQUIREMENT: &str = "Node.js 22.22.3+, 24.15.0+, or 25.9.0+";
const MANAGED_NODE_VERSION: &str = "24.21.0";

const INTERNAL_MANAGED_NODE_ARCHIVE_URL_ENV: &str = "OCM_INTERNAL_MANAGED_NODE_ARCHIVE_URL";
const INTERNAL_MANAGED_NODE_ARCHIVE_SHA256_ENV: &str = "OCM_INTERNAL_MANAGED_NODE_ARCHIVE_SHA256";
const NODE_DIST_BASE_URL: &str = "https://nodejs.org/dist";

#[derive(Clone, Debug)]
pub(crate) struct ManagedNodeToolchain {
    pub(crate) node_bin: PathBuf,
    pub(crate) npm_cli: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedNodeDistribution {
    version: String,
    asset_name: String,
    root_dir_name: String,
    archive_kind: ManagedNodeArchiveKind,
    node_relative_path: &'static str,
    npm_cli_relative_path: &'static str,
    platform_label: &'static str,
    archive_sha256: &'static str,
}

#[derive(Clone, Copy, Debug)]
enum ManagedNodeArchiveKind {
    TarGz,
    Zip,
}

#[derive(Clone, Debug)]
pub(crate) struct CommandSpec {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) path_prepend: Option<PathBuf>,
}

impl CommandSpec {
    pub(crate) fn apply_environment(
        &self,
        command: &mut Command,
        env: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let Some(path_prepend) = self.path_prepend.as_deref() else {
            return Ok(());
        };
        command.env(
            "PATH",
            prepend_to_path(
                path_prepend,
                environment_path_value(env, cfg!(windows)),
                cfg!(windows),
            )?,
        );
        Ok(())
    }
}

pub(crate) fn managed_node_fallback_supported() -> bool {
    managed_node_distribution().is_ok()
}

pub(crate) fn managed_node_fallback_detail() -> Result<String, String> {
    let distribution = managed_node_distribution()?;
    Ok(format!(
        "OCM can install a private Node.js {} toolchain for official releases on {}",
        distribution.version, distribution.platform_label
    ))
}

pub(crate) fn load_existing_managed_node_toolchain(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Option<ManagedNodeToolchain>, String> {
    let distribution = match managed_node_distribution() {
        Ok(distribution) => distribution,
        Err(_) => return Ok(None),
    };
    let root = managed_node_root(&distribution, env, cwd)?;
    // Released OCM installations use this cache name. Keep their launches and
    // installs working offline when the preferred private Node version changes.
    let previous_root = root.with_file_name(distribution.root_dir_name.replacen(
        MANAGED_NODE_VERSION,
        "24.15.0",
        1,
    ));
    for candidate in [root, previous_root] {
        if let Some((node_bin, npm_cli)) = verify_managed_node_toolchain(&candidate, &distribution)
        {
            return Ok(Some(ManagedNodeToolchain { node_bin, npm_cli }));
        }
    }
    Ok(None)
}

pub(crate) fn ensure_managed_node_toolchain(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ManagedNodeToolchain, String> {
    let distribution = managed_node_distribution()?;
    let root = managed_node_root(&distribution, env, cwd)?;
    let parent = root.parent().ok_or_else(|| {
        format!(
            "managed Node.js root has no parent: {}",
            display_path(&root)
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let lock_path = parent.join(format!(".{}.lock", distribution.root_dir_name));
    let _lock = lock_file(&lock_path, "managed Node.js installation")?;

    // Another OCM process may have completed the same installation while this
    // process waited for the lock, so validate the shared root again here.
    if let Some(toolchain) = load_existing_managed_node_toolchain(env, cwd)? {
        return Ok(toolchain);
    }

    if root.exists() {
        fs::remove_dir_all(&root).map_err(|error| {
            format!(
                "failed to clear the incomplete managed Node.js toolchain at {}: {error}",
                display_path(&root)
            )
        })?;
    }

    let stage_root = parent.join(format!(
        ".node-toolchain-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    if stage_root.exists() {
        let _ = fs::remove_dir_all(&stage_root);
    }

    let result = (|| {
        fs::create_dir_all(&stage_root).map_err(|error| error.to_string())?;
        let archive_path = stage_root.join(&distribution.asset_name);
        download_to_file(&managed_node_archive_url(&distribution, env), &archive_path)?;
        verify_file_sha256(
            &archive_path,
            &managed_node_archive_sha256(&distribution, env)?,
        )?;

        let extract_root = stage_root.join("extract");
        match distribution.archive_kind {
            ManagedNodeArchiveKind::TarGz => extract_tar_gz(&archive_path, &extract_root)?,
            ManagedNodeArchiveKind::Zip => extract_zip(&archive_path, &extract_root)?,
        }

        let extracted_root = extract_root.join(&distribution.root_dir_name);
        let Some((node_bin, npm_cli)) =
            verify_managed_node_toolchain(&extracted_root, &distribution)
        else {
            return Err(format!(
                "managed Node.js archive did not contain the expected files for {}",
                distribution.platform_label
            ));
        };

        fs::rename(&extracted_root, &root).map_err(|error| {
            format!(
                "failed to place the managed Node.js toolchain in {}: {error}",
                display_path(&root)
            )
        })?;

        let node_bin = relocate_checked_path(&node_bin, &extracted_root, &root)?;
        let npm_cli = relocate_checked_path(&npm_cli, &extracted_root, &root)?;
        Ok(ManagedNodeToolchain { node_bin, npm_cli })
    })();

    let _ = fs::remove_dir_all(&stage_root);
    result
}

pub(crate) fn managed_runtime_install_command(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<CommandSpec, String> {
    let toolchain = ensure_managed_node_toolchain(env, cwd)?;
    let path_prepend = toolchain.node_bin.parent().map(Path::to_path_buf);
    Ok(CommandSpec {
        program: display_path(&toolchain.node_bin),
        args: vec![display_path(&toolchain.npm_cli)],
        path_prepend,
    })
}

pub(crate) fn managed_runtime_launch_command(
    binary_path: &str,
    openclaw_args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
    bootstrap: bool,
) -> Result<CommandSpec, String> {
    let toolchain = if bootstrap {
        ensure_managed_node_toolchain(env, cwd)?
    } else {
        load_existing_managed_node_toolchain(env, cwd)?.ok_or_else(|| {
            format!(
                "managed Node.js is not installed yet for OpenClaw package runtimes; rerun a release flow like \"{} start\" or \"{} runtime install --channel stable\"",
                command_example(env),
                command_example(env)
            )
        })?
    };
    let path_prepend = toolchain.node_bin.parent().map(Path::to_path_buf);
    let mut args = vec![binary_path.to_string()];
    args.extend(openclaw_args.iter().cloned());
    Ok(CommandSpec {
        program: display_path(&toolchain.node_bin),
        args,
        path_prepend,
    })
}

pub(crate) fn apply_path_prepend_to_environment(
    env: &mut BTreeMap<String, String>,
    path_prepend: Option<&Path>,
) -> Result<(), String> {
    apply_path_prepend_to_environment_with_platform_semantics(env, path_prepend, cfg!(windows))
}

fn apply_path_prepend_to_environment_with_platform_semantics(
    env: &mut BTreeMap<String, String>,
    path_prepend: Option<&Path>,
    case_insensitive: bool,
) -> Result<(), String> {
    let Some(path_prepend) = path_prepend else {
        return Ok(());
    };
    let path = prepend_to_path(
        path_prepend,
        environment_path_value(env, case_insensitive),
        case_insensitive,
    )?
    .into_string()
    .map_err(|_| "managed Node.js PATH contains non-Unicode data".to_string())?;
    if case_insensitive {
        env.retain(|key, _| !key.eq_ignore_ascii_case("PATH"));
    } else {
        env.remove("PATH");
    }
    env.insert("PATH".to_string(), path);
    Ok(())
}

fn command_example(env: &BTreeMap<String, String>) -> String {
    env.get("OCM_SELF")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ocm")
        .to_string()
}

fn managed_node_distribution() -> Result<ManagedNodeDistribution, String> {
    let version = MANAGED_NODE_VERSION.to_string();
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => {
            return Err(format!(
                "OCM cannot install a private Node.js toolchain on this architecture yet: {other}"
            ));
        }
    };

    match std::env::consts::OS {
        "macos" => managed_node_distribution_for(
            &version,
            &format!("darwin-{arch}"),
            "macOS",
            ManagedNodeArchiveKind::TarGz,
            "bin/node",
            "lib/node_modules/npm/bin/npm-cli.js",
        ),
        "linux" => managed_node_distribution_for(
            &version,
            &format!("linux-{arch}"),
            "Linux",
            ManagedNodeArchiveKind::TarGz,
            "bin/node",
            "lib/node_modules/npm/bin/npm-cli.js",
        ),
        "windows" => managed_node_distribution_for(
            &version,
            &format!("win-{arch}"),
            "Windows",
            ManagedNodeArchiveKind::Zip,
            "node.exe",
            "node_modules/npm/bin/npm-cli.js",
        ),
        other => Err(format!(
            "OCM cannot install a private Node.js toolchain on this operating system yet: {other}"
        )),
    }
}

fn managed_node_distribution_for(
    version: &str,
    suffix: &str,
    platform_label: &'static str,
    archive_kind: ManagedNodeArchiveKind,
    node_relative_path: &'static str,
    npm_cli_relative_path: &'static str,
) -> Result<ManagedNodeDistribution, String> {
    let archive_sha256 = match (version, suffix) {
        ("24.21.0", "darwin-arm64") => {
            "bed7eea5325e1108f32ce5228ddd6a5f0f08a499ee42aa7442aea583702f6057"
        }
        ("24.21.0", "darwin-x64") => {
            "1462cb3b3046b815cf8ea436d3da450ec1a9f11dac7e5a46b0ada5305d7e8097"
        }
        ("24.21.0", "linux-arm64") => {
            "724282c3b43aec998aa9527380465b45d229e021b58035f5f4f63095eabfe5d5"
        }
        ("24.21.0", "linux-x64") => {
            "6e1db87ef58b8819e5d5402eff1536491b18edd8eb7bee5ef7897876e88dc5ff"
        }
        ("24.21.0", "win-arm64") => {
            "8779b1bde1d39f8d420e3b57aa657b39891af434d3de44a919044cec06785921"
        }
        ("24.21.0", "win-x64") => {
            "158f7685b44de51f6c0df1d153526cbcd3e1bc739a8dfc607721cef75de9e541"
        }
        _ => {
            return Err(format!(
                "unsupported managed Node.js distribution: v{version}-{suffix}"
            ));
        }
    };
    let extension = match archive_kind {
        ManagedNodeArchiveKind::TarGz => "tar.gz",
        ManagedNodeArchiveKind::Zip => "zip",
    };
    let root_dir_name = format!("node-v{version}-{suffix}");
    Ok(ManagedNodeDistribution {
        version: version.to_string(),
        asset_name: format!("{root_dir_name}.{extension}"),
        root_dir_name,
        archive_kind,
        node_relative_path,
        npm_cli_relative_path,
        platform_label,
        archive_sha256,
    })
}

fn managed_node_root(
    distribution: &ManagedNodeDistribution,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let stores = resolve_store_paths(env, cwd)?;
    Ok(stores
        .home
        .join("toolchains")
        .join("node")
        .join(&distribution.root_dir_name))
}

fn managed_node_archive_url(
    distribution: &ManagedNodeDistribution,
    env: &BTreeMap<String, String>,
) -> String {
    if let Some(override_url) = env
        .get(INTERNAL_MANAGED_NODE_ARCHIVE_URL_ENV)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return override_url.to_string();
    }

    format!(
        "{}/v{}/{}",
        NODE_DIST_BASE_URL, distribution.version, distribution.asset_name
    )
}

fn managed_node_archive_sha256(
    distribution: &ManagedNodeDistribution,
    env: &BTreeMap<String, String>,
) -> Result<String, String> {
    if env
        .get(INTERNAL_MANAGED_NODE_ARCHIVE_URL_ENV)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return env
            .get(INTERNAL_MANAGED_NODE_ARCHIVE_SHA256_ENV)
            .map(String::as_str)
            .map(normalize_sha256)
            .transpose()?
            .ok_or_else(|| {
                format!(
                    "{INTERNAL_MANAGED_NODE_ARCHIVE_SHA256_ENV} is required when overriding the managed Node.js archive URL"
                )
            });
    }
    Ok(distribution.archive_sha256.to_string())
}

fn verify_managed_node_toolchain(
    root: &Path,
    distribution: &ManagedNodeDistribution,
) -> Option<(PathBuf, PathBuf)> {
    let node_bin = root.join(distribution.node_relative_path);
    let npm_cli = root.join(distribution.npm_cli_relative_path);
    (node_bin.is_file() && npm_cli.is_file()).then_some((node_bin, npm_cli))
}

fn relocate_checked_path(path: &Path, from_root: &Path, to_root: &Path) -> Result<PathBuf, String> {
    let relative = path
        .strip_prefix(from_root)
        .map_err(|error| error.to_string())?;
    Ok(to_root.join(relative))
}

fn environment_path_value(
    env: &BTreeMap<String, String>,
    case_insensitive: bool,
) -> Option<&String> {
    if let Some(path) = env.get("PATH") {
        return Some(path);
    }
    if !case_insensitive {
        return None;
    }
    env.iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value)
}

fn prepend_to_path(
    path: &Path,
    current: Option<&String>,
    case_insensitive: bool,
) -> Result<OsString, String> {
    let mut paths = vec![path.to_path_buf()];
    if let Some(current) = current {
        paths.extend(std::env::split_paths(current).filter(|candidate| {
            !candidate.as_os_str().is_empty() && !paths_equal(candidate, path, case_insensitive)
        }));
    }
    std::env::join_paths(paths)
        .map_err(|error| format!("failed to add managed Node.js to PATH for OpenClaw: {error}"))
}

fn paths_equal(left: &Path, right: &Path, case_insensitive: bool) -> bool {
    left == right
        || (case_insensitive
            && left
                .to_str()
                .zip(right.to_str())
                .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right)))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::{
        apply_path_prepend_to_environment_with_platform_semantics, environment_path_value,
        prepend_to_path,
    };

    #[test]
    fn managed_node_path_reads_windows_path_case_insensitively() {
        let mut env = BTreeMap::new();
        env.insert("Path".to_string(), path_value(&["system", "tools"]));

        assert_eq!(
            environment_path_value(&env, true),
            env.get("Path"),
            "Windows Path must remain available to npm lifecycle commands"
        );
        assert_eq!(environment_path_value(&env, false), None);
    }

    #[test]
    fn managed_node_path_replaces_windows_path_without_losing_system_entries() {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), path_value(&["system", "tools"]));
        env.insert("Path".to_string(), path_value(&["stale"]));

        apply_path_prepend_to_environment_with_platform_semantics(
            &mut env,
            Some(Path::new("managed-node")),
            true,
        )
        .unwrap();

        assert!(!env.contains_key("Path"));
        assert_eq!(
            env.keys()
                .filter(|key| key.eq_ignore_ascii_case("PATH"))
                .count(),
            1
        );
        assert_eq!(
            std::env::split_paths(env.get("PATH").unwrap()).collect::<Vec<_>>(),
            vec![
                PathBuf::from("managed-node"),
                PathBuf::from("system"),
                PathBuf::from("tools")
            ]
        );
    }

    #[test]
    fn managed_node_path_preserves_system_entries_and_deduplicates_itself() {
        let managed = PathBuf::from("managed-node");
        let current = path_value(&["system", "managed-node", "tools"]);
        let combined = prepend_to_path(&managed, Some(&current), false).unwrap();
        let entries = std::env::split_paths(&combined).collect::<Vec<_>>();

        assert_eq!(
            entries,
            vec![managed, PathBuf::from("system"), PathBuf::from("tools")]
        );
    }

    #[test]
    fn managed_node_path_deduplicates_windows_case_variants() {
        let managed = PathBuf::from("Managed-Node");
        let current = path_value(&["system", "managed-node", "tools"]);
        let combined = prepend_to_path(&managed, Some(&current), true).unwrap();
        let entries = std::env::split_paths(&combined).collect::<Vec<_>>();

        assert_eq!(
            entries,
            vec![managed, PathBuf::from("system"), PathBuf::from("tools")]
        );
    }

    fn path_value(entries: &[&str]) -> String {
        std::env::join_paths(entries.iter().map(Path::new))
            .unwrap()
            .into_string()
            .unwrap()
    }
}
