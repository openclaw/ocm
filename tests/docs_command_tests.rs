mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use crate::support::{TestDir, ocm_env, run_ocm, stdout};

const DOCS: [&str; 5] = [
    "README.md",
    "npm/README.md",
    "docs/USAGE.md",
    "docs/SELF_UPDATE.md",
    "docs/OPENCLAW_RELEASE_SCENARIO_MATRIX.md",
];

/// Resolves command paths through the shipped binary's own `ocm help`.
struct Help {
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    cache: BTreeMap<Vec<String>, Option<String>>,
    _root: TestDir,
}

impl Help {
    fn new() -> Self {
        let root = TestDir::new("docs-commands");
        let cwd = root.child("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let env = ocm_env(&root);
        Self {
            cwd,
            env,
            cache: BTreeMap::new(),
            _root: root,
        }
    }

    /// The help text for `ocm help <path>`, or `None` when the CLI rejects it.
    fn text(&mut self, path: &[String]) -> Option<String> {
        if let Some(cached) = self.cache.get(path) {
            return cached.clone();
        }
        let mut args = vec!["help"];
        args.extend(path.iter().map(String::as_str));
        let output = run_ocm(&self.cwd, &self.env, &args);
        let text = output.status.success().then(|| stdout(&output));
        self.cache.insert(path.to_vec(), text.clone());
        text
    }

    /// The commands a group lists, or `None` for a command that takes no
    /// subcommand.
    fn commands(&mut self, path: &[String]) -> Option<Vec<String>> {
        let text = self.text(path)?;
        let group_usage = format!("ocm {} <command>", path.join(" "));
        let lists_commands = |line: &str| {
            let line = line.trim();
            line == group_usage || line.starts_with(&format!("{group_usage} "))
        };
        // `ocm help <command>` documents topics, not subcommands.
        let is_group = path.is_empty() || (path[0] != "help" && text.lines().any(lists_commands));
        is_group.then(|| listed_commands(&text))
    }
}

/// Every command name in a help text's sections, such as `create` or
/// `snapshot create`.
fn listed_commands(help: &str) -> Vec<String> {
    let mut section = "";
    let mut names = Vec::new();
    for line in help.lines() {
        if !line.is_empty() && !line.starts_with(' ') {
            section = line.trim_end_matches(':');
            continue;
        }
        if matches!(
            section,
            "Usage" | "Examples" | "More" | "Global options" | "Get started"
        ) || !line.starts_with("  ")
            || line.starts_with("   ")
        {
            continue;
        }
        let name = line.trim().split("  ").next().unwrap_or_default();
        if !name.is_empty() && !name.starts_with("ocm ") {
            names.push(name.to_string());
        }
    }
    names
}

fn first_words(commands: &[String]) -> BTreeSet<String> {
    commands
        .iter()
        .filter_map(|command| command.split(' ').next())
        .map(str::to_string)
        .collect()
}

fn is_word(token: &str) -> bool {
    let mut chars = token.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Checks that `ocm <tokens>` names a command the CLI accepts. Tokens after
/// the command path (names, flags, placeholders) are not examined.
fn check_command(help: &mut Help, tokens: &[String]) -> Result<(), String> {
    let mut index = 0;
    while tokens.get(index).map(String::as_str) == Some("--color") {
        index += 2;
    }
    let Some(first) = tokens.get(index) else {
        return Ok(());
    };
    if first.starts_with('@') || first == "--" {
        return Ok(());
    }
    if first.starts_with('-') {
        let top = help.commands(&[]).expect("top-level help");
        return if top.contains(first) || matches!(first.as_str(), "--help" | "-h") {
            Ok(())
        } else {
            Err(format!("`{first}` is not listed by `ocm help`"))
        };
    }

    let mut path: Vec<String> = Vec::new();
    while let Some(commands) = help.commands(&path) {
        let Some(token) = tokens.get(index).filter(|token| is_word(token)) else {
            break;
        };
        if !first_words(&commands).contains(token) {
            return Err(format!(
                "`{token}` is not a command of `ocm{}{}`",
                if path.is_empty() { "" } else { " " },
                path.join(" ")
            ));
        }
        path.push(token.clone());
        index += 1;
    }
    if path.first().map(String::as_str) == Some("help") {
        let topic: Vec<String> = tokens[index..]
            .iter()
            .take_while(|token| is_word(token))
            .cloned()
            .collect();
        if !topic.is_empty() && help.text(&topic).is_none() {
            return Err(format!("`ocm help {}` is rejected", topic.join(" ")));
        }
    }
    Ok(())
}

fn strings(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|token| token.to_string()).collect()
}

/// Splits one shell line into words, stopping at a control operator,
/// redirection or comment.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => current.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                '<' | '>' if current.is_empty() && !started => {
                    // `<env>` is a placeholder; `> file` is a redirection.
                    if chars.peek().is_some_and(char::is_ascii_alphabetic) {
                        current.push(c);
                    } else {
                        break;
                    }
                }
                '>' if current.starts_with('<') => current.push(c),
                '|' | '&' | ';' | '<' | '>' => break,
                '#' if !started && current.is_empty() => break,
                c if c.is_whitespace() => {
                    if started || !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                c => current.push(c),
            },
        }
    }
    if started || !current.is_empty() {
        words.push(current);
    }
    words
}

/// The tokens after `ocm` when `snippet` invokes the CLI, such as `ocm env
/// list` or `"$OCM_BIN" env list`.
fn invocation(snippet: &str) -> Option<Vec<String>> {
    let words = shell_words(snippet.trim().trim_start_matches("$ "));
    let mut words = words.iter().skip_while(|word| {
        word.split_once('=')
            .is_some_and(|(name, _)| is_word(name) && !name.contains('-'))
    });
    let program = words.next()?;
    matches!(program.as_str(), "ocm" | "$OCM_BIN" | "${OCM_BIN}")
        .then(|| words.map(String::to_string).collect())
}

/// Every documented invocation with its 1-based source line.
fn invocations(markdown: &str) -> Vec<(usize, Vec<String>)> {
    let mut found = Vec::new();
    let mut in_fence = false;
    let mut pending: Option<(usize, String)> = None;
    for (index, line) in markdown.lines().enumerate() {
        let number = index + 1;
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            pending = None;
            continue;
        }
        if in_fence {
            let (start, mut command) = pending.take().unwrap_or((number, String::new()));
            command.push(' ');
            if let Some(continued) = line.trim_end().strip_suffix('\\') {
                command.push_str(continued);
                pending = Some((start, command));
                continue;
            }
            command.push_str(line);
            if let Some(tokens) = invocation(&command) {
                found.push((start, tokens));
            }
        } else {
            for (position, span) in line.split('`').enumerate() {
                if position % 2 == 1
                    && let Some(tokens) = invocation(span)
                {
                    found.push((number, tokens));
                }
            }
        }
    }
    found
}

fn read(path: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
        .replace("\r\n", "\n")
}

fn section<'a>(markdown: &'a str, heading: &str) -> &'a str {
    let start = markdown
        .find(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("missing section {heading:?}"));
    let body = &markdown[start + heading.len() + 2..];
    let level = heading.split(' ').next().unwrap();
    let end = body
        .lines()
        .scan(0, |offset, line| {
            let here = *offset;
            *offset += line.len() + 1;
            Some((here, line))
        })
        .find(|(_, line)| {
            line.starts_with(level)
                && line[level.len()..].starts_with(' ')
                && !line.starts_with(&format!("{level}#"))
        })
        .map_or(body.len(), |(offset, _)| offset);
    &body[..end]
}

#[test]
fn documented_commands_are_accepted_by_the_cli() {
    let mut help = Help::new();
    let mut failures = Vec::new();
    let mut checked = 0;
    for doc in DOCS {
        for (line, tokens) in invocations(&read(doc)) {
            checked += 1;
            if let Err(reason) = check_command(&mut help, &tokens) {
                failures.push(format!("{doc}:{line}: ocm {}: {reason}", tokens.join(" ")));
            }
        }
    }
    assert!(checked > 100, "found only {checked} documented commands");
    assert!(
        failures.is_empty(),
        "documented commands the CLI rejects:\n{}",
        failures.join("\n")
    );
}

#[test]
fn command_check_rejects_removed_commands_and_accepts_current_ones() {
    let mut help = Help::new();
    for removed in [
        &["service", "list"][..],
        &["service", "list", "--json"],
        &["service", "discover"],
        &["service", "logs", "mira"],
        &["service", "adopt-global"],
        &["service", "restore-global"],
        &["env", "repair-marker", "mira"],
        &["env", "snapshot", "bogus"],
        &["help", "service", "list"],
    ] {
        assert!(
            check_command(&mut help, &strings(removed)).is_err(),
            "ocm {} must be rejected",
            removed.join(" ")
        );
    }
    for current in [
        &["service", "status", "--json"][..],
        &["service", "status", "mira"],
        &["service", "recover", "mira"],
        &["service", "refresh-daemon"],
        &["logs", "mira", "--tail", "5"],
        &["env", "doctor", "mira"],
        &["env", "cleanup", "mira", "--yes"],
        &["env", "protect", "mira", "on"],
        &["env", "snapshot", "create", "mira"],
        &["env", "artifact", "export", "mira"],
        &["--color", "always", "runtime", "list"],
        &["--version"],
        &["@mira", "--", "status"],
        &["help", "service", "recover"],
    ] {
        assert_eq!(
            check_command(&mut help, &strings(current)),
            Ok(()),
            "ocm {} must be accepted",
            current.join(" ")
        );
    }
}

#[test]
fn invocations_are_found_in_fences_continuations_and_inline_code() {
    let markdown = "\
Use `ocm service status` or `\"$OCM_BIN\" logs <env>`, not `brew install ocm`.

```bash
$ ocm env create mira \\
  --launcher stable
OCM_HOME=/tmp/x ocm service install mira | cat
# ocm service list
echo ocm service list
```
";
    assert_eq!(
        invocations(markdown),
        vec![
            (1, strings(&["service", "status"])),
            (1, strings(&["logs", "<env>"])),
            (
                4,
                strings(&["env", "create", "mira", "--launcher", "stable"])
            ),
            (6, strings(&["service", "install", "mira"])),
        ]
    );
}

#[test]
fn usage_command_map_matches_the_help_listing() {
    let usage = read("docs/USAGE.md");
    let map = section(&usage, "## Command map");
    let mut help = Help::new();

    let mut mapped: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();
    let mut group = None;
    for line in map.lines() {
        if let Some(heading) = line.strip_prefix("### ") {
            let heading = heading.trim_matches('`');
            let path = if heading == "Top-level commands" {
                Vec::new()
            } else {
                vec![heading.to_string()]
            };
            mapped.entry(path.clone()).or_default();
            group = Some(path);
        } else if let Some(items) = line.strip_prefix("- ") {
            let path = group.clone().expect("a command map item outside a group");
            let entries = mapped.get_mut(&path).unwrap();
            // "snapshot create, list" names `snapshot create` and `snapshot list`.
            let mut prefix = None;
            for item in items.split(',') {
                let item = item.trim().trim_matches('`');
                let item = if path.is_empty() {
                    item.strip_prefix("ocm ").unwrap_or(item)
                } else {
                    item
                };
                match (item.split_once(' '), prefix) {
                    (Some((group, _)), _) => {
                        prefix = Some(group);
                        entries.insert(item.to_string());
                    }
                    (None, Some(group)) => {
                        entries.insert(format!("{group} {item}"));
                    }
                    (None, None) => {
                        entries.insert(item.to_string());
                    }
                }
            }
        }
    }

    for (path, documented) in &mapped {
        let topic = format!("ocm help {}", path.join(" "));
        let topic = topic.trim_end();
        let listed: BTreeSet<String> = help
            .commands(path)
            .unwrap_or_else(|| panic!("`{topic}` lists no commands"))
            .into_iter()
            .collect();
        assert_eq!(
            documented, &listed,
            "the Command map differs from `{topic}`"
        );
    }

    let top = help.commands(&[]).expect("top-level help");
    for command in top.iter().filter(|command| !command.starts_with('-')) {
        let path = vec![command.clone()];
        if help.commands(&path).is_some() {
            assert!(
                mapped.contains_key(&path),
                "the Command map has no section for `ocm {command}`, which lists subcommands"
            );
        }
    }
}

#[test]
fn usage_safety_notes_claim_only_enforced_safeguards() {
    let usage = read("docs/USAGE.md");
    let notes = section(&usage, "## Safety notes").to_lowercase();
    assert!(notes.contains("protected environments are not removed unless forced"));
    for removed in ["marker", "free port"] {
        assert!(
            !notes.contains(removed),
            "the Safety notes still mention {removed:?}, which OCM does not enforce"
        );
    }
}
