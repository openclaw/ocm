use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) struct NpmInstallation {
    location: NpmLocation,
}

enum NpmLocation {
    Global(PathBuf),
    Local(PathBuf),
    Ephemeral,
}

impl NpmInstallation {
    pub(crate) fn from_executable(executable: &Path) -> Option<Self> {
        let executable = executable.canonicalize().ok()?;
        let bin = executable.parent()?;
        let target = bin.parent()?;
        let vendor = target.parent()?;
        let package = vendor.parent()?;
        if executable.file_name()? != "ocm"
            || bin.file_name()? != "bin"
            || vendor.file_name()? != "vendor"
            || !matches!(
                target.file_name()?.to_str()?,
                "aarch64-apple-darwin" | "x86_64-apple-darwin" | "x86_64-unknown-linux-gnu"
            )
        {
            return None;
        }
        let manifest: serde_json::Value =
            serde_json::from_reader(File::open(package.join("package.json")).ok()?.take(65536))
                .ok()?;
        if manifest.get("name")?.as_str()? != "@openclaw/ocm" {
            return None;
        }

        // Alias directories and the manifest version are not ownership identities.
        // npm can hoist a payload, and a running daemon can outlive an upgrade.
        let mut location = NpmLocation::Local(package.to_path_buf());
        for ancestor in package.ancestors() {
            if ancestor
                .file_name()
                .is_some_and(|name| name == "node_modules")
                && let Some(parent) = ancestor.parent()
            {
                if parent
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == "_npx")
                {
                    return Some(Self {
                        location: NpmLocation::Ephemeral,
                    });
                }
                location = if parent.file_name().is_some_and(|name| name == "lib")
                    && let Some(prefix) = parent.parent()
                    && let Ok(launcher) = prefix.join("bin/ocm").canonicalize()
                    && launcher.starts_with(ancestor)
                    && launcher.file_name().is_some_and(|name| name == "ocm.cjs")
                {
                    NpmLocation::Global(prefix.to_path_buf())
                } else {
                    NpmLocation::Local(parent.to_path_buf())
                };
            }
        }
        Some(Self { location })
    }

    pub(crate) fn update_guidance(&self) -> String {
        match &self.location {
            NpmLocation::Global(prefix) => format!(
                "npm manages this ocm installation; run `npm install --global @openclaw/ocm@latest` with the same npm prefix ({})",
                prefix.display()
            ),
            NpmLocation::Local(project) => format!(
                "npm manages this ocm installation; run `npm install @openclaw/ocm@latest` in its owning project ({})",
                project.display()
            ),
            NpmLocation::Ephemeral =>
                "npx manages this temporary ocm installation; rerun with `npx --yes @openclaw/ocm@latest` or install OCM globally for background services".to_string(),
        }
    }

    pub(crate) fn require_durable(&self) -> Result<(), String> {
        if matches!(self.location, NpmLocation::Ephemeral) {
            return Err(
                "cannot create or rebind the OCM background service from a temporary npx cache; install with `npm install --global @openclaw/ocm`, then rerun using the installed `ocm` command"
                    .to_string(),
            );
        }
        Ok(())
    }
}
