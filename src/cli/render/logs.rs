use super::RenderProfile;
use crate::infra::terminal::{KeyValueRow, Tone, paint, render_key_value_card};
use crate::logs::LogComponentSummary;
use time::OffsetDateTime;

pub fn log_header(
    env_name: &str,
    components: &[LogComponentSummary],
    tail_lines: Option<usize>,
    follow: bool,
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return Vec::new();
    }

    let mut lines = vec![paint(
        &format!("Logs {}", env_name),
        Tone::Strong,
        profile.color,
    )];
    let mut rows = Vec::new();
    if components.len() == 1 {
        let component = &components[0];
        rows.push(KeyValueRow::plain("Stream", component.stream.clone()));
        rows.push(KeyValueRow::plain("Source", component.source_kind.clone()));
        rows.push(KeyValueRow::plain("Path", component.path.clone()));
    } else {
        rows.push(KeyValueRow::plain("Streams", "stdout + stderr"));
        for component in components {
            rows.push(KeyValueRow::plain(
                format!("{} source", component.stream),
                component.source_kind.clone(),
            ));
            rows.push(KeyValueRow::plain(
                format!("{} path", component.stream),
                component.path.clone(),
            ));
        }
    }
    rows.push(KeyValueRow::plain(
        "Mode",
        if follow {
            "follow".to_string()
        } else {
            "snapshot".to_string()
        },
    ));
    rows.push(KeyValueRow::plain(
        "Tail",
        tail_lines
            .map(|value| value.to_string())
            .unwrap_or_else(|| "all".to_string()),
    ));
    lines.extend(render_key_value_card("Active log", &rows, profile.color));
    lines.push(String::new());
    lines
}

pub fn render_log_text(text: &str, profile: RenderProfile) -> String {
    if !profile.pretty {
        return text.to_string();
    }

    text.split_inclusive('\n')
        .map(|line| render_log_line(line, profile))
        .collect::<String>()
}

fn render_log_line(line: &str, profile: RenderProfile) -> String {
    let has_newline = line.ends_with('\n');
    let body = line.trim_end_matches('\n');
    if body.is_empty() {
        return line.to_string();
    }

    let rendered = parse_bracketed_line(body)
        .or_else(|| parse_leveled_line(body))
        .map(|parsed| format_parsed_line(parsed, profile))
        .unwrap_or_else(|| {
            let tone = heuristic_line_tone(body);
            if matches!(tone, Tone::Plain) {
                body.to_string()
            } else {
                paint(body, tone, profile.color)
            }
        });

    if has_newline {
        format!("{rendered}\n")
    } else {
        rendered
    }
}

#[derive(Clone, Copy)]
struct ParsedLogLine<'a> {
    timestamp: Option<&'a str>,
    level: Option<&'a str>,
    source: Option<&'a str>,
    message: &'a str,
    bracket_source: bool,
}

fn parse_bracketed_line(line: &str) -> Option<ParsedLogLine<'_>> {
    let (timestamp, rest) = line.split_once(' ')?;
    if !looks_like_timestamp(timestamp) {
        return None;
    }
    let rest = rest.strip_prefix('[')?;
    let end = rest.find(']')?;
    let source = &rest[..end];
    let message = rest[end + 1..].trim_start();
    Some(ParsedLogLine {
        timestamp: Some(timestamp),
        level: None,
        source: Some(source),
        message,
        bracket_source: true,
    })
}

fn parse_leveled_line(line: &str) -> Option<ParsedLogLine<'_>> {
    let (timestamp, rest) = line.split_once(' ')?;
    if !looks_like_timestamp(timestamp) {
        return None;
    }

    let mut parts = rest.splitn(3, ' ');
    let level = parts.next()?;
    if !is_log_level(level) {
        return None;
    }
    let source = parts.next();
    let message = parts.next().unwrap_or("").trim_start();
    Some(ParsedLogLine {
        timestamp: Some(timestamp),
        level: Some(level),
        source,
        message,
        bracket_source: false,
    })
}

fn format_parsed_line(parsed: ParsedLogLine<'_>, profile: RenderProfile) -> String {
    let mut parts = Vec::new();
    let content_tone = parsed
        .level
        .map(level_tone)
        .unwrap_or_else(|| heuristic_line_tone(parsed.message));
    if let Some(timestamp) = parsed.timestamp {
        parts.push(paint(
            &format_log_timestamp(timestamp),
            Tone::Muted,
            profile.color,
        ));
    }
    if let Some(level) = parsed.level {
        parts.push(paint(level, level_tone(level), profile.color));
    }
    if let Some(source) = parsed.source {
        let source_label = if parsed.bracket_source {
            format!("[{source}]")
        } else {
            source.to_string()
        };
        let source_tone = parsed.level.map(level_tone).unwrap_or_else(|| {
            let tone = heuristic_line_tone(parsed.message);
            if matches!(tone, Tone::Plain) {
                Tone::Accent
            } else {
                tone
            }
        });
        parts.push(paint(&source_label, source_tone, profile.color));
    }
    if !parsed.message.is_empty() {
        parts.push(paint(parsed.message, content_tone, profile.color));
    }
    parts.join(" ")
}

fn format_log_timestamp(raw: &str) -> String {
    OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .map(|value| {
            value
                .format(
                    &time::format_description::parse_borrowed::<1>(
                        "[hour]:[minute]:[second][offset_hour sign:mandatory]:[offset_minute]",
                    )
                    .unwrap(),
                )
                .unwrap_or_else(|_| raw.to_string())
        })
        .unwrap_or_else(|_| raw.to_string())
}

fn looks_like_timestamp(value: &str) -> bool {
    value
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
        && value.contains(':')
}

fn is_log_level(value: &str) -> bool {
    matches!(
        value,
        "trace" | "debug" | "info" | "warn" | "error" | "fatal"
    )
}

fn level_tone(level: &str) -> Tone {
    match level {
        "error" | "fatal" => Tone::Danger,
        "warn" => Tone::Warning,
        "debug" | "trace" => Tone::Muted,
        "info" => Tone::Accent,
        _ => Tone::Plain,
    }
}

fn heuristic_line_tone(line: &str) -> Tone {
    let lower = line.to_ascii_lowercase();
    if lower.contains("fatal")
        || lower.contains("error")
        || lower.contains(" failed")
        || lower.contains("exception")
        || lower.contains("panic")
    {
        Tone::Danger
    } else if lower.contains("warn") || lower.contains("warning") {
        Tone::Warning
    } else {
        Tone::Plain
    }
}

#[cfg(test)]
mod tests {
    use super::{log_header, render_log_text};
    use crate::cli::render::RenderProfile;
    use crate::logs::LogComponentSummary;

    #[test]
    fn log_header_pretty_uses_cards() {
        let lines = log_header(
            "demo",
            &[LogComponentSummary {
                stream: "stdout".to_string(),
                source_kind: "gateway".to_string(),
                path: "/tmp/demo/.openclaw/logs/gateway.log".to_string(),
            }],
            Some(50),
            false,
            RenderProfile::pretty(false),
        );
        assert!(lines.iter().any(|line| line.contains("Logs demo")));
        assert!(lines.iter().any(|line| line.contains("gateway.log")));
        assert!(lines.iter().any(|line| line.contains("snapshot")));
    }

    #[test]
    fn render_log_text_pretty_formats_common_openclaw_log_shapes() {
        let rendered = render_log_text(
            concat!(
                "2026-04-20T00:13:45.497+01:00 [agents/tool-images] Image resized\n",
                "04:42:38+00:00 error gateway connect failed\n"
            ),
            RenderProfile::pretty(true),
        );
        assert!(rendered.contains("00:13:45+01:00"));
        assert!(rendered.contains("[agents/tool-images]"));
        assert!(rendered.contains("\u{1b}[31merror\u{1b}[0m"));
        assert!(rendered.contains("\u{1b}[31mgateway\u{1b}[0m"));
        assert!(rendered.contains("\u{1b}[31mconnect failed\u{1b}[0m"));
    }
}
