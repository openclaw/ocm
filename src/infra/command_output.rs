use serde_json::Value;

const MAX_SUMMARY_LINES: usize = 12;
const MAX_SUMMARY_CHARS: usize = 4_096;
const SENSITIVE_ASSIGNMENTS: [&str; 14] = [
    "access_token=",
    "access_token:",
    "refresh_token=",
    "refresh_token:",
    "api_key=",
    "api_key:",
    "api-key=",
    "api-key:",
    "password=",
    "password:",
    "secret=",
    "secret:",
    "token=",
    "token:",
];

/// Returns a bounded, redacted diagnostic from failed command output.
pub(crate) fn summarize_command_failure(primary: &str, secondary: &str) -> Option<String> {
    // Machine output owns the precise failure when a command emits both a
    // generic human preamble and a structured error envelope.
    if let Some(message) = [primary, secondary]
        .into_iter()
        .find_map(structured_error_message)
    {
        return bounded_summary(message.lines());
    }

    let mut fallback = Vec::new();
    for text in [primary, secondary] {
        let lines = command_lines(text, false);
        if !lines.is_empty() {
            return bounded_summary(lines.iter().map(String::as_str));
        }
        fallback.extend(command_lines(text, true));
    }
    bounded_summary(fallback.iter().map(String::as_str))
}

fn structured_error_message(text: &str) -> Option<String> {
    let extract = |value: Value| match value.get("error")? {
        Value::String(message) => Some(message.clone()),
        Value::Object(error) => ["message", "detail", "cause"]
            .into_iter()
            .find_map(|key| error.get(key).and_then(Value::as_str).map(str::to_string)),
        _ => None,
    };
    serde_json::from_str::<Value>(text.trim())
        .ok()
        .and_then(&extract)
        .or_else(|| {
            text.lines()
                .rev()
                .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
                .find_map(extract)
        })
}

fn command_lines(text: &str, include_npm_chatter: bool) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            include_npm_chatter
                || (!line.starts_with("npm notice") && !line.starts_with("npm warn"))
        })
        .map(str::to_string)
        .collect()
}

fn bounded_summary<'a>(lines: impl Iterator<Item = &'a str>) -> Option<String> {
    let lines = lines.map(redact_obvious_secrets).collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }

    // Keep both the producer's first cause and the command's final context.
    let selected = if lines.len() > MAX_SUMMARY_LINES {
        let omitted = lines.len() - (MAX_SUMMARY_LINES - 1);
        let mut selected = lines[..4].to_vec();
        selected.push(format!("... {omitted} lines omitted ..."));
        selected.extend_from_slice(&lines[lines.len() - 7..]);
        selected
    } else {
        lines
    };
    let summary = selected.join("\n");
    if summary.chars().count() <= MAX_SUMMARY_CHARS {
        return Some(summary);
    }
    Some(
        summary
            .chars()
            .take(MAX_SUMMARY_CHARS - 3)
            .chain("...".chars())
            .collect(),
    )
}

fn redact_obvious_secrets(line: &str) -> String {
    let lower_line = line.to_ascii_lowercase();
    if let Some((marker, start)) = ["authorization:", "authorization="]
        .into_iter()
        .find_map(|marker| lower_line.find(marker).map(|start| (marker, start)))
    {
        // Authorization schemes vary, so retaining any following word can
        // expose the credential for Basic, Digest, or another scheme.
        let prefix = redact_assignments(line[..start].trim_end());
        let separator = if prefix.is_empty() { "" } else { " " };
        return format!("{prefix}{separator}{marker}<redacted>");
    }

    redact_assignments(line)
}

fn redact_assignments(line: &str) -> String {
    let mut redact_next = false;
    line.split_whitespace()
        .map(|word| {
            // Bearer values are separate words; assignments keep their key so
            // the resulting diagnostic still explains which input was used.
            if redact_next && word.eq_ignore_ascii_case("bearer") {
                return word.to_string();
            }
            if std::mem::take(&mut redact_next) {
                return "<redacted>".to_string();
            }
            let lower = word.to_ascii_lowercase();
            if lower == "bearer" || lower.ends_with("bearer") {
                redact_next = true;
                return word.to_string();
            }
            let assignment = SENSITIVE_ASSIGNMENTS
                .into_iter()
                .find_map(|marker| lower.find(marker).map(|start| (marker, start)));
            let Some((marker, start)) = assignment else {
                return word.to_string();
            };
            let value_start = start + marker.len();
            if marker.ends_with(':') && value_start == word.len() {
                redact_next = true;
            }
            format!("{}<redacted>", &word[..value_start])
        })
        .collect::<Vec<_>>()
        .join(" ")
}
