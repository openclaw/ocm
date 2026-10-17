use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::RuntimeMeta;
use crate::host::verify_package_runtime_node;
use crate::managed_node::{NodeRequirement, managed_runtime_launch_command};
use crate::runtime::releases::is_official_openclaw_releases_url;

#[derive(Clone, Debug)]
pub(crate) struct RuntimeLaunchSpec {
    pub(crate) runtime_binary_path: String,
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) path_prepend: Option<PathBuf>,
}

pub(crate) fn resolve_runtime_launch(
    meta: &RuntimeMeta,
    openclaw_args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
    bootstrap_managed_node: bool,
) -> Result<RuntimeLaunchSpec, String> {
    let requirement = if is_openclaw_package_runtime(meta) {
        Some(NodeRequirement::from_manifest(
            &Path::new(&meta.binary_path).with_file_name("package.json"),
        )?)
    } else {
        None
    };
    if let Some(requirement) = requirement
        && verify_package_runtime_node(env, &requirement).is_err()
    {
        let managed = managed_runtime_launch_command(
            &meta.binary_path,
            openclaw_args,
            env,
            cwd,
            bootstrap_managed_node,
            &requirement,
        )?;
        return Ok(RuntimeLaunchSpec {
            runtime_binary_path: meta.binary_path.clone(),
            program: managed.program,
            args: managed.args,
            path_prepend: managed.path_prepend,
        });
    }

    Ok(RuntimeLaunchSpec {
        runtime_binary_path: meta.binary_path.clone(),
        program: meta.binary_path.clone(),
        args: openclaw_args.to_vec(),
        path_prepend: None,
    })
}

pub(crate) fn is_openclaw_package_runtime(meta: &RuntimeMeta) -> bool {
    meta.source_kind == super::RuntimeSourceKind::Installed
        && PathBuf::from(&meta.binary_path)
            .ends_with(Path::new("node_modules/openclaw/openclaw.mjs"))
}

pub(crate) fn is_official_openclaw_package_runtime(
    meta: &RuntimeMeta,
    env: &BTreeMap<String, String>,
) -> bool {
    is_openclaw_package_runtime(meta)
        && is_official_openclaw_releases_url(meta.source_manifest_url.as_deref(), env)
}
