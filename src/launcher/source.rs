use super::LauncherMeta;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) fn launcher_source_root(launcher: &LauncherMeta) -> Option<PathBuf> {
    let root = package_root(launcher, false)?;
    let runner = fs::canonicalize(root.join("scripts/run-node.mjs")).ok()?;
    (runner.starts_with(&root) && runner.is_file()).then_some(root)
}

/// Identify the package behind a literal native OpenClaw launcher, including an
/// installed package that does not contain the development checkout runner and
/// an installed `openclaw` executable that links to the package entrypoint, as
/// `ocm migrate` records for a global npm install.
pub(crate) fn launcher_package_root(launcher: &LauncherMeta) -> Option<PathBuf> {
    package_root(launcher, true)
}

fn package_root(launcher: &LauncherMeta, installed_executable: bool) -> Option<PathBuf> {
    let cwd = launcher.cwd.as_deref().map(Path::new);
    let words = super::parse_literal_launcher_command(&launcher.command)?;
    let (program_path, args) = words.split_first()?;
    let program = Path::new(program_path).file_name()?.to_str()?;
    let root = match (program, args) {
        ("pnpm" | "pnpm.cmd", [script]) if script == "openclaw" => cwd?.to_path_buf(),
        ("node" | "node.exe", [entry]) => entrypoint_package_dir(entry, cwd)?,
        // A bare program name is found through PATH when the launcher runs, so
        // only an explicit path identifies the executable here.
        (_, []) if installed_executable && Path::new(program_path).parent()? != Path::new("") => {
            entrypoint_package_dir(program_path, cwd)?
        }
        _ => return None,
    };
    let root = fs::canonicalize(root).ok()?;
    let package = read_source_json(&root, "package.json", 1024 * 1024).ok()?;
    if package.get("name")?.as_str()? != "openclaw" {
        return None;
    }
    if matches!(program, "pnpm" | "pnpm.cmd")
        && package.pointer("/scripts/openclaw")?.as_str()? != "node scripts/run-node.mjs"
    {
        return None;
    }
    Some(root)
}

fn entrypoint_package_dir(entry: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let entry = Path::new(entry);
    let entry = if entry.is_absolute() {
        entry.to_path_buf()
    } else {
        cwd?.join(entry)
    };
    let entry = fs::canonicalize(entry).ok()?;
    if entry.file_name()? != "openclaw.mjs" {
        return None;
    }
    Some(entry.parent()?.to_path_buf())
}

pub(crate) fn read_source_json(root: &Path, relative: &str, limit: u64) -> Result<Value, String> {
    let path = fs::canonicalize(root.join(relative))
        .map_err(|_| format!("{relative} is missing or unreadable"))?;
    if !path.starts_with(root) {
        return Err(format!("{relative} resolves outside the checkout"));
    }
    if !path.is_file() {
        return Err(format!("{relative} is not a regular file"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|_| format!("{relative} could not be read"))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{relative} is too large"));
    }
    serde_json::from_slice(&bytes).map_err(|_| format!("{relative} is invalid JSON"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::{launcher_package_root, launcher_source_root};
    use crate::launcher::LauncherMeta;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use time::OffsetDateTime;

    fn launcher(command: &str) -> LauncherMeta {
        LauncherMeta {
            kind: "ocm-launcher".to_string(),
            name: "source".to_string(),
            command: command.to_string(),
            cwd: None,
            description: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn write_package(dir: &Path, name: &str) {
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join("openclaw.mjs"), "#!/bin/sh\n").unwrap();
        fs::write(dir.join("scripts/run-node.mjs"), "").unwrap();
        fs::write(
            dir.join("package.json"),
            format!(r#"{{"name":"{name}","scripts":{{"openclaw":"node scripts/run-node.mjs"}}}}"#),
        )
        .unwrap();
    }

    #[test]
    fn installed_executable_identifies_only_the_linked_openclaw_package() {
        let root =
            std::env::temp_dir().join(format!("ocm-launcher-source-tests-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let package = root.join("lib/node_modules/openclaw");
        let other = root.join("lib/node_modules/other");
        write_package(&package, "openclaw");
        write_package(&other, "other");
        fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin/openclaw");
        symlink("../lib/node_modules/openclaw/openclaw.mjs", &executable).unwrap();
        symlink(
            "../lib/node_modules/other/openclaw.mjs",
            root.join("bin/other"),
        )
        .unwrap();
        fs::write(root.join("bin/wrapper"), "#!/bin/sh\n").unwrap();
        let executable = executable.to_string_lossy().into_owned();
        let bin = root.join("bin");

        let installed = launcher(&executable);
        assert_eq!(
            launcher_package_root(&installed),
            Some(fs::canonicalize(&package).unwrap())
        );
        // Source-checkout detection keeps its literal runner spellings.
        assert_eq!(launcher_source_root(&installed), None);
        for command in [
            "openclaw".to_string(),
            format!("{executable} --profile dev"),
            bin.join("other").to_string_lossy().into_owned(),
            bin.join("wrapper").to_string_lossy().into_owned(),
        ] {
            assert_eq!(
                launcher_package_root(&launcher(&command)),
                None,
                "{command}"
            );
        }
        fs::remove_dir_all(&root).unwrap();
    }
}
