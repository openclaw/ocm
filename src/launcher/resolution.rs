use super::LauncherMeta;
use std::path::{Path, PathBuf};

use crate::env::EnvMeta;
use crate::infra::shell::quote_posix;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectLauncherCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
}

pub fn resolve_launcher_name(
    env_meta: &EnvMeta,
    launcher_override: Option<String>,
) -> Result<String, String> {
    launcher_override
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env_meta.default_launcher.clone())
        .ok_or_else(|| {
            format!(
                "environment \"{}\" has no default launcher; use env set-launcher or pass --launcher",
                env_meta.name
            )
        })
}

pub fn build_launcher_command(launcher: &LauncherMeta, args: &[String]) -> String {
    if args.is_empty() {
        return launcher.command.clone();
    }

    let mut command = launcher.command.clone();
    let quoted = args.iter().map(|arg| quote_posix(arg)).collect::<Vec<_>>();
    command.push(' ');
    command.push_str(&quoted.join(" "));
    command
}

pub fn resolve_launcher_run_dir(launcher: &LauncherMeta, fallback_cwd: &Path) -> PathBuf {
    launcher
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback_cwd.to_path_buf())
}

pub(crate) fn resolve_direct_launcher_command(
    launcher: &LauncherMeta,
    openclaw_args: &[String],
    _fallback_cwd: &Path,
) -> Option<DirectLauncherCommand> {
    // Preserve the shell route for quoted recipes: literal words alone do not
    // establish a directly executable program (for example, the `exec` builtin).
    if launcher.command.contains(['\'', '"']) {
        return None;
    }
    let tokens = parse_literal_launcher_command(&launcher.command)?;

    Some(DirectLauncherCommand {
        program: tokens.first()?.clone(),
        args: tokens
            .into_iter()
            .skip(1)
            .chain(openclaw_args.iter().cloned())
            .collect(),
    })
}

pub(crate) fn parse_literal_launcher_command(command: &str) -> Option<Vec<String>> {
    let trimmed = command.trim();
    if trimmed.is_empty() || trimmed.contains(char::is_control) {
        return None;
    }
    // Only recognize literal words. Leave expansion, escapes and operators to the shell.
    if trimmed.contains([
        '`', '$', '|', '&', ';', '<', '>', '(', ')', '{', '}', '\\', '*', '?', '[', ']', '~', '#',
        '%', '^', '!',
    ]) || (cfg!(windows) && trimmed.contains(['\'', '"']))
    {
        return None;
    }

    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    for ch in trimmed.chars() {
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            } else {
                word.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            started = true;
        } else if ch == ' ' {
            if started {
                tokens.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(ch);
            started = true;
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        tokens.push(word);
    }
    let first = tokens.first()?;
    if first.is_empty() || first.contains('=') {
        return None;
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::{parse_literal_launcher_command, resolve_direct_launcher_command};
    use crate::launcher::LauncherMeta;
    use std::path::Path;
    use time::OffsetDateTime;

    fn sample_launcher(command: &str, cwd: Option<&str>) -> LauncherMeta {
        LauncherMeta {
            kind: "ocm-launcher".to_string(),
            name: "dev".to_string(),
            command: command.to_string(),
            cwd: cwd.map(str::to_string),
            description: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn parse_literal_launcher_command_rejects_shell_syntax() {
        assert!(parse_literal_launcher_command("FOO=bar openclaw").is_none());
        assert!(parse_literal_launcher_command("pnpm openclaw | tee log").is_none());
        assert!(parse_literal_launcher_command("openclaw 'gateway run").is_none());
        assert!(parse_literal_launcher_command("openclaw \"$ENTRY\"").is_none());
        assert!(parse_literal_launcher_command("openclaw \"$(entry)\"").is_none());
        assert!(parse_literal_launcher_command("openclaw \"`entry`\"").is_none());
        assert!(parse_literal_launcher_command("'' openclaw").is_none());
        assert!(parse_literal_launcher_command("openclaw gateway\nopenclaw status").is_none());
        assert!(parse_literal_launcher_command(r"openclaw foo\ bar").is_none());
        assert!(parse_literal_launcher_command("openclaw --config ~/openclaw.json").is_none());
        assert!(parse_literal_launcher_command("openclaw plugins/*.mjs").is_none());
        assert!(parse_literal_launcher_command("openclaw plugin?.mjs").is_none());
        assert!(parse_literal_launcher_command("openclaw plugins/[ab].mjs").is_none());
        assert!(parse_literal_launcher_command("openclaw gateway # foreground").is_none());
        assert!(parse_literal_launcher_command("openclaw %OPENCLAW_ARGS%").is_none());
        assert!(parse_literal_launcher_command("openclaw ^&").is_none());
        assert!(parse_literal_launcher_command("openclaw !OPENCLAW_ARGS!").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn parse_literal_launcher_command_preserves_literal_quoted_words() {
        assert_eq!(
            parse_literal_launcher_command(
                r#"'/path to/node' "/source tree/openclaw.mjs" '' pre"mid dle"post"#
            ),
            Some(vec![
                "/path to/node".to_string(),
                "/source tree/openclaw.mjs".to_string(),
                "".to_string(),
                "premid dlepost".to_string(),
            ])
        );
        let entry = "/source's tree/openclaw.mjs";
        assert_eq!(
            parse_literal_launcher_command(&format!(
                "node {}",
                crate::infra::shell::quote_posix(entry)
            )),
            Some(vec!["node".to_string(), entry.to_string()])
        );
    }

    #[cfg(windows)]
    #[test]
    fn parse_literal_launcher_command_keeps_windows_quotes_opaque() {
        assert!(parse_literal_launcher_command(r#"node "C:/source tree/openclaw.mjs""#).is_none());
        assert!(parse_literal_launcher_command("node 'openclaw.mjs'").is_none());
    }

    #[test]
    fn resolve_direct_launcher_command_uses_tokens_for_simple_commands() {
        let launcher = sample_launcher("openclaw --profile dev", None);
        let command = resolve_direct_launcher_command(
            &launcher,
            &["gateway".to_string(), "run".to_string()],
            Path::new("/tmp/fallback"),
        )
        .unwrap();

        assert_eq!(command.program, "openclaw");
        assert_eq!(command.args, vec!["--profile", "dev", "gateway", "run"]);
    }

    #[test]
    fn resolve_direct_launcher_command_preserves_quoted_shell_recipes() {
        for recipe in [
            "exec \"node\" 'openclaw.mjs'",
            "node '/source tree/openclaw.mjs'",
        ] {
            assert!(
                resolve_direct_launcher_command(
                    &sample_launcher(recipe, None),
                    &[],
                    Path::new("/tmp")
                )
                .is_none()
            );
        }
    }

    #[test]
    fn resolve_direct_launcher_command_keeps_package_manager_launchers_as_is() {
        let launcher = sample_launcher("pnpm openclaw --profile dev", None);
        let command = resolve_direct_launcher_command(
            &launcher,
            &[
                "gateway".to_string(),
                "run".to_string(),
                "--port".to_string(),
                "18900".to_string(),
            ],
            Path::new("/tmp/fallback"),
        )
        .unwrap();

        assert_eq!(command.program, "pnpm");
        assert_eq!(
            command.args,
            vec![
                "openclaw",
                "--profile",
                "dev",
                "gateway",
                "run",
                "--port",
                "18900",
            ]
        );
    }
}
