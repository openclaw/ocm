use super::LauncherMeta;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::infra::shell::quote_posix;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectLauncherCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
}

pub fn build_launcher_command(launcher: &LauncherMeta, args: &[String]) -> String {
    if args.is_empty() {
        return launcher.command.clone();
    }

    let command = &launcher.command;
    let quoted = args
        .iter()
        .map(|arg| quote_posix(arg))
        .collect::<Vec<_>>()
        .join(" ");
    // Appended text would sit inside a trailing comment, or behind a dangling
    // backslash that escapes the separating space, so the arguments follow the
    // last word of the recipe's last command instead. `cmd /C` has neither form.
    match cfg!(unix).then(|| posix_recipe_tail(command)).flatten() {
        Some(RecipeTail::Comment(word_end)) => {
            let (head, comments) = command.split_at(word_end);
            let separator = if comments.starts_with(char::is_whitespace) {
                ""
            } else {
                " "
            };
            format!("{head} {quoted}{separator}{comments}")
        }
        Some(RecipeTail::Backslash(word_end)) => format!("{} {quoted}", &command[..word_end]),
        None => format!("{command} {quoted}"),
    }
}

/// How a POSIX shell recipe ends after its last word, given as the byte offset
/// just past that word.
#[derive(Debug, PartialEq, Eq)]
enum RecipeTail {
    /// Only blanks and `#` comments follow the last word.
    Comment(usize),
    /// An unquoted backslash ends the recipe.
    Backslash(usize),
}

/// Finds a trailing comment or dangling backslash after the last word of a
/// POSIX shell recipe. The scan reads only plain words, quotes, backslash
/// escapes, `$NAME` and plain `${...}` expansions, operators and comments; a `#`
/// starts a comment only at the start of a token outside quotes and
/// expansions. Anything that can hide or move token boundaries (command and
/// process substitution, backquotes, `$'...'`, here-documents, line
/// continuations, `((`, `[[`, parentheses inside a word, `${...}` holding
/// quotes or nested expansions) and unterminated quotes return `None`, so those
/// recipes keep the plain append.
fn posix_recipe_tail(command: &str) -> Option<RecipeTail> {
    let bytes = command.as_bytes();
    let mut index = 0;
    let mut word_end = 0;
    let mut word_start = true;
    while let Some(&byte) = bytes.get(index) {
        let next = bytes.get(index + 1);
        let span_end = match byte {
            b'\'' => Some(index + 2 + bytes[index + 1..].iter().position(|&b| b == b'\'')?),
            b'"' => Some(skip_double_quotes(bytes, index)?),
            b'$' if next == Some(&b'{') => Some(skip_parameter(bytes, index)?),
            b'`' => return None,
            b'$' if matches!(next, Some(b'(' | b'\'')) => return None,
            b'<' | b'>' | b'(' if matches!(next, Some(b'(')) => return None,
            // Bash reads `[[ ... ]]` regexes and extended globs such as
            // `@(a|#b)` with their own rules for parentheses and `#`.
            b'[' if word_start && next == Some(&b'[') => return None,
            b'(' | b')' if !word_start && !ends_token(bytes[index - 1]) => return None,
            b'<' if next == Some(&b'<') => return None,
            b'\\' if next == Some(&b'\n') => return None,
            b'\\' if next.is_none() => {
                return (word_end > 0).then_some(RecipeTail::Backslash(word_end));
            }
            b'\\' => Some(index + 2),
            b'#' if word_start => match bytes[index..].iter().position(|&b| b == b'\n') {
                // The comment ends at this newline; scanning resumes there.
                Some(offset) => {
                    index += offset;
                    continue;
                }
                None => return (word_end > 0).then_some(RecipeTail::Comment(word_end)),
            },
            _ => None,
        };
        if let Some(end) = span_end {
            index = end;
            word_end = end;
            word_start = false;
            continue;
        }
        if !matches!(byte, b' ' | b'\t' | b'\n') {
            word_end = index + 1;
        }
        word_start = ends_token(byte);
        index += 1;
    }
    None
}

/// The offset just past the double-quoted span opening at `start`, or `None`
/// when it is unterminated or holds a substitution, backquote or line
/// continuation.
fn skip_double_quotes(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    loop {
        match (*bytes.get(index)?, bytes.get(index + 1)) {
            (b'"', _) => return Some(index + 1),
            (b'`', _) | (b'$', Some(b'(')) | (b'\\', Some(b'\n')) => return None,
            (b'$', Some(b'{')) => index = skip_parameter(bytes, index)?,
            (b'\\', _) => index += 2,
            _ => index += 1,
        }
    }
}

/// The offset just past the `${...}` expansion opening at `start`, or `None`
/// unless it closes before any quote, `$`, backquote, backslash, brace or
/// newline.
fn skip_parameter(bytes: &[u8], start: usize) -> Option<usize> {
    let offset = bytes[start + 2..].iter().position(|&byte| {
        matches!(
            byte,
            b'}' | b'\'' | b'"' | b'`' | b'$' | b'\\' | b'{' | b'\n'
        )
    })?;
    let end = start + 2 + offset;
    (bytes[end] == b'}').then_some(end + 1)
}

/// Blanks and operator characters end a shell token.
fn ends_token(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>'
    )
}

pub fn resolve_launcher_run_dir(launcher: &LauncherMeta, fallback_cwd: &Path) -> PathBuf {
    launcher
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback_cwd.to_path_buf())
}

/// Why the launcher's recorded working directory cannot be entered, if it
/// cannot. A launcher without one runs in the caller's directory.
pub fn launcher_working_directory_issue(launcher: &LauncherMeta) -> Option<String> {
    working_directory_issue(Path::new(launcher.cwd.as_deref()?))
}

/// Why `dir` cannot be a child process's working directory, if it cannot.
pub fn working_directory_issue(dir: &Path) -> Option<String> {
    match fs::metadata(dir) {
        Ok(metadata) if metadata.is_dir() => None,
        Ok(_) => Some(format!(
            "working directory is not a directory: {}",
            dir.display()
        )),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Some(format!(
                "working directory does not exist: {}",
                dir.display()
            ))
        }
        Err(error) => Some(format!(
            "working directory could not be read: {}: {error}",
            dir.display()
        )),
    }
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
    // Unix shell execution and sourcing recipes need the shell even when every
    // word is literal. Keep execution separate from parsing for source inspection.
    if cfg!(unix)
        && matches!(
            tokens.first()?.as_str(),
            "exec" | "eval" | "." | "source" | "builtin" | "command"
        )
    {
        return None;
    }

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
    if cfg!(windows) && trimmed.contains(['\'', '"']) {
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
            } else if delimiter == '"' && matches!(ch, '$' | '`' | '\\') {
                // Double quotes still permit expansion and escapes.
                return None;
            } else {
                word.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            started = true;
        } else if matches!(
            ch,
            '`' | '$'
                | '|'
                | '&'
                | ';'
                | '<'
                | '>'
                | '('
                | ')'
                | '{'
                | '}'
                | '\\'
                | '*'
                | '?'
                | '['
                | ']'
                | '~'
                | '#'
                | '%'
                | '^'
                | '!'
        ) {
            // Keep unquoted shell syntax opaque; quoted data is literal.
            return None;
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
        assert!(parse_literal_launcher_command(r#"openclaw "foo\ bar""#).is_none());
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
        let symbols = "/source #1 & data (copy) [x] {a,b} * ? ~ % ^ !/openclaw.mjs";
        for delimiter in ['\'', '"'] {
            assert_eq!(
                parse_literal_launcher_command(&format!("node {delimiter}{symbols}{delimiter}")),
                Some(vec!["node".to_string(), symbols.to_string()])
            );
        }
        let literal = r"/source $(entry) `entry` \ data/openclaw.mjs";
        assert_eq!(
            parse_literal_launcher_command(&format!("node '{literal}'")),
            Some(vec!["node".to_string(), literal.to_string()])
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
            ". '/source/wrapper.sh'",
            "source '/source/wrapper.sh'",
            "builtin source '/source/wrapper.sh'",
            "command '/source/openclaw'",
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
    fn resolve_direct_launcher_command_keeps_builtin_shell_semantics_on_unix() {
        for builtin in ["exec", "eval", ".", "source", "builtin", "command"] {
            let recipe = format!("{builtin} openclaw --profile dev");
            assert_eq!(
                parse_literal_launcher_command(&recipe).unwrap(),
                vec![builtin, "openclaw", "--profile", "dev"]
            );
            let command = resolve_direct_launcher_command(
                &sample_launcher(&recipe, None),
                &["gateway".to_string()],
                Path::new("/tmp"),
            );
            if cfg!(unix) {
                assert!(command.is_none());
            } else {
                let command = command.unwrap();
                assert_eq!(command.program, builtin);
                assert_eq!(
                    command.args,
                    vec!["openclaw", "--profile", "dev", "gateway"]
                );
            }
        }

        let command = resolve_direct_launcher_command(
            &sample_launcher("builtin source /source/wrapper.sh", None),
            &["gateway".to_string()],
            Path::new("/tmp"),
        );
        if cfg!(unix) {
            assert!(command.is_none());
        } else {
            let command = command.unwrap();
            assert_eq!(command.program, "builtin");
            assert_eq!(
                command.args,
                vec!["source", "/source/wrapper.sh", "gateway"]
            );
        }

        for program in [
            "./exec",
            "./eval",
            "./source",
            "./builtin",
            "./command",
            "source-tool",
            "builtin-tool",
            "command-tool",
            "/usr/local/bin/builtin",
            "/usr/local/bin/command",
            "./wrapper.sh",
            "/bin/sh",
        ] {
            let command = resolve_direct_launcher_command(
                &sample_launcher(&format!("{program} openclaw"), None),
                &[],
                Path::new("/tmp"),
            )
            .unwrap();
            assert_eq!(command.program, program);
            assert_eq!(command.args, vec!["openclaw"]);
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
