use crate::infra::shell::{render_init_script, resolve_shell_name};

use super::Cli;

impl Cli {
    pub(super) fn handle_init_command(
        &self,
        shell: &str,
        args: Vec<String>,
    ) -> Result<i32, String> {
        let shell = if shell.is_empty() {
            resolve_shell_name(None, &self.env)?
        } else {
            shell.to_string()
        };
        if !matches!(shell.as_str(), "bash" | "fish" | "sh" | "zsh") {
            return Err(format!("unsupported init shell: {shell}"));
        }
        Self::assert_no_extra_args(&args)?;
        self.stdout_text(&render_init_script(&self.command_example(), &shell)?)?;
        Ok(0)
    }
}
