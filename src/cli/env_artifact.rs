use std::io;
use std::path::Path;

use super::Cli;

impl Cli {
    pub(super) fn handle_env_artifact(&self, args: Vec<String>) -> Result<i32, String> {
        if args.first().map(String::as_str) != Some("export") {
            return Err("expected env artifact export".to_string());
        }
        let (args, path) = Self::consume_option(args[1..].to_vec(), "--path")?;
        let (args, max_bytes) = Self::consume_option(args, "--max-bytes")?;
        let Some(name) = args.first() else {
            return Err("environment name is required".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;
        let path = path.ok_or_else(|| "--path is required".to_string())?;
        let max_bytes = max_bytes.ok_or_else(|| "--max-bytes is required".to_string())?;
        if max_bytes.is_empty() || !max_bytes.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("--max-bytes must be a nonnegative decimal integer".to_string());
        }
        let max_bytes = max_bytes
            .parse::<u64>()
            .map_err(|_| "--max-bytes is too large".to_string())?;

        // Unlike human-oriented receipts, a broken output stream invalidates
        // this export. Do not swallow write errors or mix metadata into stdout.
        self.environment_service().export_artifact(
            name,
            Path::new(&path),
            max_bytes,
            &mut io::stdout().lock(),
        )?;
        Ok(0)
    }
}
