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
/// POSIX shell recipe. A `#` starts a comment only at the start of a token
/// outside quotes, expansions and substitutions. Constructs this scan does not
/// model (here-documents, `$'...'`, a `case` statement inside a command
/// substitution, nesting deeper than `MAX_SPAN_NESTING`) and unterminated
/// quotes return `None`, so those recipes keep the plain append.
fn posix_recipe_tail(command: &str) -> Option<RecipeTail> {
    match scan_tokens(command.as_bytes(), 0, false, 0)? {
        (_, 0) => None,
        (ScanEnd::Comment, word_end) => Some(RecipeTail::Comment(word_end)),
        (ScanEnd::Backslash, word_end) => Some(RecipeTail::Backslash(word_end)),
        _ => None,
    }
}

const MAX_SPAN_NESTING: usize = 32;

#[derive(Debug, PartialEq, Eq)]
enum ScanEnd {
    /// The text ended after a word or blanks.
    Text,
    /// The text ended in a `#` comment.
    Comment,
    /// The text ended in an unquoted backslash.
    Backslash,
    /// The `)` closing a command substitution; the offset is just past it.
    Closed(usize),
}

/// Scans shell tokens from `start` to the end of the text or, inside a `$(`
/// substitution, to its closing `)`. Returns how the scan ended and the offset
/// just past the last word.
fn scan_tokens(
    bytes: &[u8],
    start: usize,
    in_substitution: bool,
    nesting: usize,
) -> Option<(ScanEnd, usize)> {
    let mut index = start;
    let mut word_end = start;
    let mut word_start = true;
    let mut open_parens = 0usize;
    while let Some(&byte) = bytes.get(index) {
        if opens_span(bytes, index) {
            index = skip_span(bytes, index, nesting + 1)?;
            word_end = index;
            word_start = false;
            continue;
        }
        let next = bytes.get(index + 1);
        match byte {
            b'$' if next == Some(&b'\'') => return None,
            b'<' if next == Some(&b'<') => return None,
            // A line continuation joins the lines without ending the token.
            b'\\' if next == Some(&b'\n') => {
                index += 2;
                continue;
            }
            b'\\' if next.is_none() => {
                return (!in_substitution).then_some((ScanEnd::Backslash, word_end));
            }
            b'\\' => {
                index += 2;
                word_end = index;
                word_start = false;
                continue;
            }
            b'#' if word_start => match bytes[index..].iter().position(|&byte| byte == b'\n') {
                // The comment ends at this newline; scanning resumes there.
                Some(offset) => {
                    index += offset;
                    continue;
                }
                None => return (!in_substitution).then_some((ScanEnd::Comment, word_end)),
            },
            // `case` statement patterns would unbalance the parentheses. As an
            // argument, `case` is an ordinary word.
            b'c' if in_substitution
                && is_word_at(bytes, index, b"case")
                && in_command_position(&bytes[start..index]) =>
            {
                return None;
            }
            b'(' if in_substitution => open_parens += 1,
            b')' if in_substitution => match open_parens.checked_sub(1) {
                Some(remaining) => open_parens = remaining,
                None => return Some((ScanEnd::Closed(index + 1), word_end)),
            },
            _ => {}
        }
        if !matches!(byte, b' ' | b'\t' | b'\n') {
            word_end = index + 1;
        }
        word_start = ends_token(byte);
        index += 1;
    }
    (!in_substitution).then_some((ScanEnd::Text, word_end))
}

/// Whether `word` is a whole token at `index`.
fn is_word_at(bytes: &[u8], index: usize, word: &[u8]) -> bool {
    bytes[index..].starts_with(word)
        && (index == 0 || ends_token(bytes[index - 1]))
        && bytes
            .get(index + word.len())
            .is_none_or(|&next| ends_token(next))
}

/// Whether a token following `before` starts a command: nothing, an operator
/// or a newline precedes it, or a reserved word that a command follows.
fn in_command_position(before: &[u8]) -> bool {
    let Some(last) = before
        .iter()
        .rposition(|&byte| !matches!(byte, b' ' | b'\t'))
    else {
        return true;
    };
    if ends_token(before[last]) {
        return true;
    }
    let first = before[..last]
        .iter()
        .rposition(|&byte| ends_token(byte))
        .map_or(0, |at| at + 1);
    matches!(
        &before[first..=last],
        b"if" | b"then" | b"else" | b"elif" | b"while" | b"until" | b"do" | b"!" | b"{"
    )
}

/// Blanks and operator characters end a shell token.
fn ends_token(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>'
    )
}

/// Whether a quoted span, `${...}` expansion or `$(...)` substitution opens at
/// `index`.
fn opens_span(bytes: &[u8], index: usize) -> bool {
    match bytes[index] {
        b'\'' | b'"' | b'`' => true,
        b'$' => matches!(bytes.get(index + 1), Some(b'(' | b'{')),
        _ => false,
    }
}

/// The offset just past the span that `opens_span` found at `start`, or `None`
/// when it is unterminated, nested too deeply or holds a construct the scan
/// does not model.
fn skip_span(bytes: &[u8], start: usize, nesting: usize) -> Option<usize> {
    if nesting > MAX_SPAN_NESTING {
        return None;
    }
    let (close, mut index) = match (bytes[start], bytes.get(start + 1)) {
        (b'\'', _) => {
            let end = bytes[start + 1..].iter().position(|&byte| byte == b'\'')?;
            return Some(start + end + 2);
        }
        // Arithmetic expansion: only its parentheses matter here.
        (b'$', Some(b'(')) if bytes.get(start + 2) == Some(&b'(') => {
            let mut depth = 0usize;
            for (offset, &byte) in bytes[start + 1..].iter().enumerate() {
                match byte {
                    b'(' => depth += 1,
                    b')' if depth == 1 => return Some(start + offset + 2),
                    b')' => depth -= 1,
                    _ => {}
                }
            }
            return None;
        }
        (b'$', Some(b'(')) => {
            return match scan_tokens(bytes, start + 2, true, nesting)? {
                (ScanEnd::Closed(end), _) => Some(end),
                _ => None,
            };
        }
        (b'$', _) => (b'}', start + 2),
        (delimiter, _) => (delimiter, start + 1),
    };
    // Double quotes, backquotes and `${...}`.
    loop {
        let byte = *bytes.get(index)?;
        if byte == close {
            return Some(index + 1);
        }
        if byte == b'\\' {
            index += 2;
        } else if close != b'`' && opens_span(bytes, index) && (close == b'}' || byte != b'\'') {
            index = skip_span(bytes, index, nesting + 1)?;
        } else {
            index += 1;
        }
    }
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
