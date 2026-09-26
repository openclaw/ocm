use super::LauncherMeta;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) fn launcher_source_root(launcher: &LauncherMeta) -> Option<PathBuf> {
    let cwd = launcher.cwd.as_deref().map(Path::new);
    let words = super::parse_literal_launcher_command(&launcher.command)?;
    let (program, args) = words.split_first()?;
    let program = Path::new(program).file_name()?.to_str()?;
    let root = match (program, args) {
        ("pnpm" | "pnpm.cmd", [script]) if script == "openclaw" => cwd?.to_path_buf(),
        ("node" | "node.exe", [entry]) => {
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
            entry.parent()?.to_path_buf()
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
    let runner = fs::canonicalize(root.join("scripts/run-node.mjs")).ok()?;
    (runner.starts_with(&root) && runner.is_file()).then_some(root)
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
