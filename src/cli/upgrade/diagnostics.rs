use std::fmt;

use serde_json::Value;

use crate::infra::command_output::{
    bounded_summary, structured_error_message, summarize_command_failure,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateFailureKind {
    Configuration,
    ManagedRuntime,
    Launch,
    Other,
}

#[derive(Debug)]
pub(super) struct CandidateFailure {
    pub(super) kind: CandidateFailureKind,
    message: String,
}

impl CandidateFailure {
    pub(super) fn launch(message: String) -> Self {
        Self {
            kind: CandidateFailureKind::Launch,
            message: bounded_summary(message.lines())
                .unwrap_or_else(|| "candidate managed Codex preflight could not start".to_string()),
        }
    }

    pub(super) fn from_output(code: Option<i32>, stdout: &str, stderr: &str) -> Self {
        let (kind, detail) = summarize_candidate_output(stdout, stderr);
        Self {
            kind,
            message: format!(
                "candidate managed Codex preflight failed: exited with code {}: {detail}",
                code.unwrap_or(1),
            ),
        }
    }
}

impl fmt::Display for CandidateFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn doctor_report(text: &str) -> Option<Value> {
    let has_findings = |value: &Value| {
        value
            .get("findings")
            .and_then(Value::as_array)
            .is_some_and(|findings| !findings.is_empty())
    };
    serde_json::from_str::<Value>(text.trim())
        .ok()
        .filter(&has_findings)
        .or_else(|| {
            text.lines()
                .rev()
                .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
                .find(has_findings)
        })
}

fn finding_kind(findings: &[Value]) -> CandidateFailureKind {
    let mut kind = None;
    for finding in findings {
        let next = match finding.get("severity").and_then(Value::as_str) {
            Some("info" | "warning") => continue,
            Some("error") => match finding.get("checkId").and_then(Value::as_str) {
                Some("core/doctor/final-config-validation") => CandidateFailureKind::Configuration,
                Some("codex/managed-app-server") => CandidateFailureKind::ManagedRuntime,
                _ => CandidateFailureKind::Other,
            },
            _ => CandidateFailureKind::Other,
        };
        kind = Some(match kind {
            None => next,
            Some(previous) if previous == next => next,
            Some(_) => CandidateFailureKind::Other,
        });
    }
    kind.unwrap_or(CandidateFailureKind::Other)
}

fn finding_lines(finding: &Value) -> Vec<String> {
    let text = |key| {
        finding
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let mut lines = vec![format!(
        "{} ({}): {}",
        text("checkId").unwrap_or("unknown check"),
        text("severity").unwrap_or("unknown severity"),
        text("message").unwrap_or("no readable finding message"),
    )];
    // These scalar fields are the Doctor diagnostic contract. Never serialize
    // arbitrary plugin-owned data or extra fields from a finding.
    for (key, label) in [
        ("path", "path"),
        ("requirement", "requirement"),
        ("fixHint", "candidate hint before recovery"),
    ] {
        if let Some(value) = text(key) {
            lines.push(format!("{label}: {value}"));
        }
    }
    lines
}

fn readable_command_detail(text: &str) -> Option<String> {
    if let Some(error) = structured_error_message(text) {
        return bounded_summary(error.lines());
    }
    bounded_summary(
        plain_command_output(text)?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty()),
    )
}

fn plain_command_output(text: &str) -> Option<&str> {
    // Unknown or malformed machine output is not a license to dump its JSON.
    // Keep eligible streams intact until chatter filtering and stream selection;
    // truncating early could make an omission marker masquerade as the cause.
    let machine_output = serde_json::from_str::<Value>(text.trim()).is_ok()
        || text.lines().any(looks_like_machine_output);
    if machine_output {
        return None;
    }
    Some(text)
}

fn looks_like_machine_output(line: &str) -> bool {
    let line = line.trim();
    if line.starts_with('{') {
        return true;
    }
    if let Some(rest) = line.strip_prefix('[') {
        // A log label such as [ERROR] is not a JSON array. Require both a
        // plain label and following text before treating this as a diagnostic.
        return !rest.split_once(']').is_some_and(|(label, message)| {
            !message.trim().is_empty()
                && label.chars().any(|ch| ch.is_ascii_alphabetic())
                && label
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || " _-:.".contains(ch))
                && serde_json::from_str::<Value>(&format!("[{label}]")).is_err()
        });
    }
    if let Some(rest) = line.strip_prefix('"') {
        // Quoted executable names can prefix ordinary launch errors. A
        // quoted field followed by a JSON value still belongs to machine output.
        return !rest.split_once('"').is_some_and(|(_, suffix)| {
            let detail = suffix
                .trim_start()
                .strip_prefix(':')
                .unwrap_or(suffix)
                .trim();
            !detail.is_empty()
                && !detail.starts_with(['{', '[', '"', ',', '}'])
                && serde_json::from_str::<Value>(detail.trim_end_matches(',')).is_err()
        });
    }
    false
}

fn summarize_candidate_output(stdout: &str, stderr: &str) -> (CandidateFailureKind, String) {
    for (report_text, context_text, context_label) in
        [(stdout, stderr, "stderr"), (stderr, stdout, "stdout")]
    {
        if let Some(report) = doctor_report(report_text) {
            let findings = report["findings"]
                .as_array()
                .expect("Doctor findings array");
            let kind = finding_kind(findings);
            let has_errors = findings
                .iter()
                .any(|finding| finding.get("severity").and_then(Value::as_str) == Some("error"));
            let mut lines = findings
                .iter()
                .filter(|finding| {
                    !has_errors
                        || !matches!(
                            finding.get("severity").and_then(Value::as_str),
                            Some("info" | "warning")
                        )
                })
                .flat_map(finding_lines)
                .collect::<Vec<_>>();
            if let Some(context) = readable_command_detail(context_text) {
                // Secondary chatter cannot consume the finding's entire budget.
                // Redact before shortening it, then bound the combined report.
                let mut shortened = context.chars().take(512).collect::<String>();
                if context.chars().count() > 512 {
                    shortened.push_str("...");
                }
                lines.push(format!("{context_label}: {shortened}"));
            }
            let detail = bounded_summary(lines.iter().flat_map(|line| line.lines()))
                .unwrap_or_else(|| "Doctor returned no readable finding details".to_string());
            return (kind, detail);
        }
    }

    let detail = [stdout, stderr]
        .into_iter()
        .find_map(structured_error_message)
        .and_then(|message| bounded_summary(message.lines()))
        .or_else(|| {
            summarize_command_failure(
                plain_command_output(stderr).unwrap_or_default(),
                plain_command_output(stdout).unwrap_or_default(),
            )
        })
        .unwrap_or_else(|| {
            if stdout.trim().is_empty() && stderr.trim().is_empty() {
                "no output".to_string()
            } else {
                "candidate output did not contain a readable diagnostic".to_string()
            }
        });
    (CandidateFailureKind::Other, detail)
}

#[cfg(test)]
mod tests {
    use super::{CandidateFailureKind, summarize_candidate_output};
    use serde_json::json;

    #[test]
    fn doctor_findings_survive_stderr_without_expanding_private_fields() {
        let report = json!({"findings": [{
            "checkId": "core/doctor/final-config-validation", "severity": "error",
            "message": "Invalid configuration. Authorization: Bearer confidential-value",
            "path": "https://user:password@example.test/config",
            "fixHint": "Repair target configuration using token=private-token",
            "privateData": {"secret": "unrelated-private-payload"},
        }]})
        .to_string();
        let (kind, detail) =
            summarize_candidate_output(&report, "Warning: optional hook unavailable");
        assert_eq!(kind, CandidateFailureKind::Configuration);
        assert!(detail.contains("core/doctor/final-config-validation"));
        assert!(detail.contains("https://<redacted>@example.test/config"));
        assert!(detail.contains("candidate hint before recovery: Repair target configuration"));
        assert!(detail.contains("Warning: optional hook unavailable"));
        for secret in [
            "confidential-value",
            "user:password",
            "private-token",
            "unrelated-private-payload",
        ] {
            assert!(!detail.contains(secret), "{detail}");
        }
    }

    #[test]
    fn only_recognized_error_findings_choose_runtime_repair() {
        let runtime = json!({"checkId":"codex/managed-app-server","severity":"error","message":"version mismatch"});
        let config = json!({"checkId":"core/doctor/final-config-validation","severity":"error","message":"bad config"});
        let unknown =
            json!({"checkId":"plugin/other","severity":"error","message":"plugin failure"});
        let warning = json!({"checkId":"codex/managed-app-server","severity":"warning","message":"version warning"});
        for (findings, expected) in [
            (vec![runtime.clone()], CandidateFailureKind::ManagedRuntime),
            (vec![config.clone()], CandidateFailureKind::Configuration),
            (vec![runtime.clone(), config], CandidateFailureKind::Other),
            (vec![runtime, unknown], CandidateFailureKind::Other),
            (vec![warning], CandidateFailureKind::Other),
        ] {
            let report = json!({"findings":findings}).to_string();
            assert_eq!(summarize_candidate_output(&report, "").0, expected);
        }
    }

    #[test]
    fn malformed_and_unknown_json_are_not_dumped_as_diagnostics() {
        for output in [
            r#"{"privateData":{"secret":"do-not-display"}}"#,
            r#"{"findings": broken, "secret":"do-not-display"}"#,
            r#"[{"secret":"do-not-display"}]"#,
            r#""privateData": {"secret":"do-not-display"}"#,
            r#""secret": "do-not-display","#,
        ] {
            let (kind, detail) =
                summarize_candidate_output(output, "candidate exited unexpectedly");
            assert_eq!(kind, CandidateFailureKind::Other);
            assert_eq!(detail, "candidate exited unexpectedly");
            assert!(!detail.contains("do-not-display"));
        }
        let (_, detail) = summarize_candidate_output(
            r#"{"error":{"message":"useful cause token=secret"}}"#,
            "generic stderr",
        );
        assert_eq!(detail, "useful cause token=<redacted>");
    }

    #[test]
    fn ordinary_bracketed_and_quoted_errors_remain_readable() {
        for output in [
            "[ERROR] missing dependency",
            r#""/candidate/openclaw": permission denied"#,
            r#""C:\candidate\openclaw.exe": permission denied"#,
        ] {
            for (stdout, stderr) in [(output, ""), ("", output)] {
                let (kind, detail) = summarize_candidate_output(stdout, stderr);
                assert_eq!(kind, CandidateFailureKind::Other);
                assert_eq!(detail, output);
            }
        }
        assert_eq!(
            summarize_candidate_output("", "[ERROR] dependency token=private-token").1,
            "[ERROR] dependency token=<redacted>"
        );
        let npm_warning = "npm warn optional dependency unavailable";
        assert_eq!(
            summarize_candidate_output("actual candidate failure", npm_warning).1,
            "actual candidate failure"
        );
        assert_eq!(summarize_candidate_output("", npm_warning).1, npm_warning);
        let many_warnings = (0..20)
            .map(|index| format!("npm warn optional warning {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            summarize_candidate_output("actual candidate failure", &many_warnings).1,
            "actual candidate failure"
        );
    }

    #[test]
    fn doctor_diagnostics_keep_existing_line_and_character_bounds() {
        let findings = (0..20)
            .map(|index| {
                json!({
                    "checkId":"codex/managed-app-server", "severity":"error",
                "message":format!("cause-{index}\n{} token=private\ncontinued finding", "x".repeat(400)),
                    "path":format!("/candidate/{index}"),
                    "fixHint":format!("action-{index}"),
                })
            })
            .collect::<Vec<_>>();
        let (_, detail) = summarize_candidate_output(
            &json!({"findings":findings}).to_string(),
            &"z".repeat(8_192),
        );
        assert!(detail.lines().count() <= 12, "{detail}");
        assert!(detail.chars().count() <= 4_096);
        assert!(detail.contains("cause-0"));
        assert!(detail.contains("action-19"));
        assert!(!detail.contains("private"));
    }
}
