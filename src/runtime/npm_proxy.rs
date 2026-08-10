use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use tar::{Archive, Builder};
use tempfile::{NamedTempFile, tempdir};

pub(crate) const INTERNAL_NPM_PROXY_REAL_BIN_ENV: &str = "OCM_INTERNAL_NPM_PROXY_REAL_BIN";
pub(crate) const INTERNAL_NPM_PROXY_WORKSPACE_DIRS_ENV: &str =
    "OCM_INTERNAL_NPM_PROXY_WORKSPACE_DIRS";
pub(crate) const INTERNAL_NPM_PROXY_WORKSPACE_VERSIONS_ENV: &str =
    "OCM_INTERNAL_NPM_PROXY_WORKSPACE_VERSIONS";

pub fn is_internal_npm_proxy(env: &BTreeMap<String, String>) -> bool {
    env.get(INTERNAL_NPM_PROXY_REAL_BIN_ENV)
        .is_some_and(|value| !value.trim().is_empty())
}

fn resolve_path(cwd: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn workspace_pack_request(
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Option<PathBuf>, String> {
    if args.first().map(String::as_str) != Some("pack") {
        return Ok(None);
    }
    let Some(package_arg) = args.get(1).filter(|value| !value.starts_with('-')) else {
        return Ok(None);
    };
    let package_dir = resolve_path(cwd, package_arg)
        .canonicalize()
        .map_err(|error| format!("failed to resolve npm pack directory {package_arg}: {error}"))?;
    let workspace_dirs = env
        .get(INTERNAL_NPM_PROXY_WORKSPACE_DIRS_ENV)
        .map(|value| std::env::split_paths(value).collect::<Vec<_>>())
        .unwrap_or_default();
    if !workspace_dirs.into_iter().any(|dir| {
        dir.canonicalize()
            .is_ok_and(|candidate| candidate == package_dir)
    }) {
        return Ok(None);
    }
    let destination = args
        .iter()
        .position(|value| value == "--pack-destination")
        .and_then(|index| args.get(index + 1))
        .ok_or_else(|| "workspace npm pack requires --pack-destination".to_string())?;
    Ok(Some(resolve_path(cwd, destination)))
}

fn tarballs_in(dir: &Path) -> Result<BTreeSet<PathBuf>, String> {
    Ok(fs::read_dir(dir)
        .map_err(|error| {
            format!(
                "failed to read workspace npm pack destination {}: {error}",
                dir.display()
            )
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("tgz"))
        .collect())
}

fn rewrite_workspace_dependency_versions(
    package: &mut serde_json::Value,
    versions: &BTreeMap<String, String>,
) -> Result<usize, String> {
    let mut rewritten = 0;
    for section in [
        "dependencies",
        "optionalDependencies",
        "peerDependencies",
        "devDependencies",
    ] {
        let Some(dependencies) = package
            .get_mut(section)
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };
        for (name, spec_value) in dependencies {
            let Some(spec) = spec_value.as_str() else {
                continue;
            };
            if !spec.starts_with("workspace:") {
                continue;
            }
            let version = versions.get(name).ok_or_else(|| {
                format!("workspace package archive references unconfigured dependency {name}")
            })?;
            *spec_value = serde_json::Value::String(version.clone());
            rewritten += 1;
        }
    }
    Ok(rewritten)
}

fn patch_workspace_package_archive(
    archive_path: &Path,
    versions: &BTreeMap<String, String>,
) -> Result<(), String> {
    let unpacked = tempdir().map_err(|error| error.to_string())?;
    let archive_file = fs::File::open(archive_path).map_err(|error| {
        format!(
            "failed to open workspace package archive {}: {error}",
            archive_path.display()
        )
    })?;
    Archive::new(GzDecoder::new(archive_file))
        .unpack(unpacked.path())
        .map_err(|error| {
            format!(
                "failed to unpack workspace package archive {}: {error}",
                archive_path.display()
            )
        })?;
    let package_dir = unpacked.path().join("package");
    let package_json_path = package_dir.join("package.json");
    let raw = fs::read_to_string(&package_json_path).map_err(|error| {
        format!(
            "failed to read packed workspace package.json at {}: {error}",
            package_json_path.display()
        )
    })?;
    let mut package: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
        format!(
            "failed to parse packed workspace package.json at {}: {error}",
            package_json_path.display()
        )
    })?;
    if rewrite_workspace_dependency_versions(&mut package, versions)? == 0 {
        return Ok(());
    }
    fs::write(
        &package_json_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&package).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| {
        format!(
            "failed to rewrite packed workspace package.json at {}: {error}",
            package_json_path.display()
        )
    })?;

    let parent = archive_path
        .parent()
        .ok_or_else(|| "workspace package archive has no parent directory".to_string())?;
    let rewritten = NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    let output = rewritten.reopen().map_err(|error| error.to_string())?;
    let encoder = GzEncoder::new(output, Compression::default());
    let mut builder = Builder::new(encoder);
    builder.follow_symlinks(false);
    builder
        .append_dir_all("package", &package_dir)
        .map_err(|error| error.to_string())?;
    let encoder = builder.into_inner().map_err(|error| error.to_string())?;
    encoder.finish().map_err(|error| error.to_string())?;
    fs::copy(rewritten.path(), archive_path).map_err(|error| {
        format!(
            "failed to replace workspace package archive {}: {error}",
            archive_path.display()
        )
    })?;
    Ok(())
}

pub fn run_internal_npm_proxy(
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<i32, String> {
    let real_npm = env
        .get(INTERNAL_NPM_PROXY_REAL_BIN_ENV)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "internal npm proxy is missing its real npm program".to_string())?;
    let pack_destination = workspace_pack_request(args, env, cwd)?;
    let before = pack_destination
        .as_deref()
        .map(tarballs_in)
        .transpose()?
        .unwrap_or_default();

    let mut command = Command::new(real_npm);
    command.args(args).current_dir(cwd);
    for (key, value) in env {
        if ![
            INTERNAL_NPM_PROXY_REAL_BIN_ENV,
            INTERNAL_NPM_PROXY_WORKSPACE_DIRS_ENV,
            INTERNAL_NPM_PROXY_WORKSPACE_VERSIONS_ENV,
        ]
        .contains(&key.as_str())
        {
            command.env(key, value);
        }
    }
    for key in [
        INTERNAL_NPM_PROXY_REAL_BIN_ENV,
        INTERNAL_NPM_PROXY_WORKSPACE_DIRS_ENV,
        INTERNAL_NPM_PROXY_WORKSPACE_VERSIONS_ENV,
    ] {
        command.env_remove(key);
    }
    let status = command
        .status()
        .map_err(|error| format!("failed to run real npm program {real_npm}: {error}"))?;
    let code = status.code().unwrap_or(1);
    if !status.success() {
        return Ok(code);
    }

    if let Some(pack_destination) = pack_destination {
        let after = tarballs_in(&pack_destination)?;
        let mut created = after.difference(&before).cloned().collect::<Vec<_>>();
        if created.len() != 1 {
            return Err(format!(
                "workspace npm pack created {} new archives in {}; expected one",
                created.len(),
                pack_destination.display()
            ));
        }
        let versions_raw = env
            .get(INTERNAL_NPM_PROXY_WORKSPACE_VERSIONS_ENV)
            .ok_or_else(|| "internal npm proxy is missing workspace versions".to_string())?;
        let versions = serde_json::from_str::<BTreeMap<String, String>>(versions_raw)
            .map_err(|error| format!("failed to parse internal workspace versions: {error}"))?;
        patch_workspace_package_archive(&created.remove(0), &versions)?;
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::patch_workspace_package_archive;
    use flate2::{Compression, read::GzDecoder, write::GzEncoder};
    use std::collections::BTreeMap;
    use std::fs;
    use tar::{Archive, Builder};

    #[test]
    fn workspace_archive_rewrites_transitive_workspace_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let package_dir = root.path().join("source/package");
        fs::create_dir_all(&package_dir).unwrap();
        fs::write(
            package_dir.join("package.json"),
            br#"{"name":"@openclaw/session-url-contract","version":"0.0.0-private","dependencies":{"@openclaw/normalization-core":"workspace:*"}}"#,
        )
        .unwrap();
        fs::write(package_dir.join("index.js"), "export {};\n").unwrap();
        let archive_path = root.path().join("workspace.tgz");
        let output = fs::File::create(&archive_path).unwrap();
        let mut builder = Builder::new(GzEncoder::new(output, Compression::default()));
        builder.append_dir_all("package", &package_dir).unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        patch_workspace_package_archive(
            &archive_path,
            &BTreeMap::from([(
                "@openclaw/normalization-core".to_string(),
                "0.0.0-private".to_string(),
            )]),
        )
        .unwrap();

        let unpacked = tempfile::tempdir().unwrap();
        Archive::new(GzDecoder::new(fs::File::open(&archive_path).unwrap()))
            .unpack(unpacked.path())
            .unwrap();
        let package: serde_json::Value = serde_json::from_slice(
            &fs::read(unpacked.path().join("package/package.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            package["dependencies"]["@openclaw/normalization-core"],
            "0.0.0-private"
        );
        assert!(unpacked.path().join("package/index.js").is_file());
    }
}
