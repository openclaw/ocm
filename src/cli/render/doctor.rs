use crate::host::{HostCheckSummary, HostDoctorSummary, HostToolFixSummary};
use crate::infra::terminal::{KeyValueRow, Tone, paint, render_key_value_card};

use super::RenderProfile;

pub fn host_doctor(
    summary: &HostDoctorSummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return host_doctor_raw(summary);
    }

    let mut lines = vec![paint("Host doctor", Tone::Strong, profile.color)];
    lines.push(String::new());

    let mut overview = vec![
        KeyValueRow::new(
            "Official releases",
            if summary.official_release_ready {
                "baseline ready"
            } else {
                "blocked"
            },
            if summary.official_release_ready {
                Tone::Success
            } else {
                Tone::Danger
            },
        ),
        KeyValueRow::plain("Required issues", summary.required_issues.to_string()),
        KeyValueRow::plain("Recommended gaps", summary.recommended_gaps.to_string()),
    ];
    let host_tool_gaps = summary.official_release_ready
        && summary
            .checks
            .iter()
            .any(|check| check.category == "official-release" && check.status != "ok");
    if summary.healthy && host_tool_gaps {
        overview.push(KeyValueRow::warning(
            "Status",
            "OCM can manage private release tools; compatibility is checked for each selected package.",
        ));
    } else if summary.healthy {
        overview.push(KeyValueRow::success(
            "Status",
            "Baseline tools are available; compatibility is checked for each selected OpenClaw package.",
        ));
    } else {
        overview.push(KeyValueRow::danger(
            "Status",
            "Install the missing required tools before using official OpenClaw releases.",
        ));
    }
    lines.extend(render_key_value_card("Overview", &overview, profile.color));

    for (title, category) in [
        ("Official releases", "official-release"),
        ("Common extras", "common-features"),
        ("Local workflows", "local-workflows"),
        ("Browser", "browser"),
        ("Background services", "background-services"),
    ] {
        let rows = summary
            .checks
            .iter()
            .filter(|check| check.category == category)
            .map(host_check_row)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            continue;
        }
        lines.push(String::new());
        lines.extend(render_key_value_card(title, &rows, profile.color));
    }

    let mut next = vec![KeyValueRow::accent(
        "Check again",
        format!("{command_example} doctor host"),
    )];
    if !summary.healthy {
        next.push(KeyValueRow::warning(
            "Official releases",
            format!("{command_example} start"),
        ));
    }
    next.push(KeyValueRow::muted(
        "Local fallback",
        format!(
            "{command_example} start luna --command 'pnpm openclaw' --cwd /path/to/openclaw --no-service"
        ),
    ));
    lines.push(String::new());
    lines.extend(render_key_value_card("Next", &next, profile.color));

    lines
}

pub fn host_tool_fixed(
    summary: &HostToolFixSummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return host_tool_fixed_raw(summary);
    }

    let title = if summary.changed {
        format!("{} ready", summary.tool)
    } else {
        format!("{} already available", summary.tool)
    };
    let mut lines = vec![paint(&title, Tone::Strong, profile.color), String::new()];

    let mut rows = vec![
        KeyValueRow::plain("Tool", summary.tool.clone()),
        KeyValueRow::new(
            "Status",
            if summary.changed {
                "installed"
            } else {
                "ready"
            },
            Tone::Success,
        ),
    ];
    if let Some(manager) = summary.manager.as_deref() {
        rows.push(KeyValueRow::plain("Manager", manager.to_string()));
    }
    if let Some(version) = summary.version.as_deref() {
        rows.push(KeyValueRow::success("Version", version.to_string()));
    }
    rows.push(KeyValueRow::muted("Detail", summary.detail.clone()));
    lines.extend(render_key_value_card("Overview", &rows, profile.color));

    lines.push(String::new());
    lines.extend(render_key_value_card(
        "Next",
        &[
            KeyValueRow::accent("Check host", format!("{command_example} doctor host")),
            KeyValueRow::muted("Start quickly", format!("{command_example} start")),
        ],
        profile.color,
    ));

    lines
}

fn host_doctor_raw(summary: &HostDoctorSummary) -> Vec<String> {
    let mut lines = vec![
        format!("healthy: {}", summary.healthy),
        format!("officialReleaseReady: {}", summary.official_release_ready),
        format!("requiredIssues: {}", summary.required_issues),
        format!("recommendedGaps: {}", summary.recommended_gaps),
    ];

    for check in &summary.checks {
        let mut bits = vec![
            format!("category={}", check.category),
            format!("name={}", check.name),
            format!("level={}", check.level),
            format!("status={}", check.status),
            format!("available={}", check.available),
        ];
        if let Some(version) = check.version.as_deref() {
            bits.push(format!("version={version}"));
        }
        if let Some(detail) = check.detail.as_deref() {
            bits.push(format!("detail={detail}"));
        }
        if let Some(suggestion) = check.suggestion.as_deref() {
            bits.push(format!("suggestion={suggestion}"));
        }
        lines.push(format!("check: {}", bits.join("  ")));
    }

    lines
}

fn host_tool_fixed_raw(summary: &HostToolFixSummary) -> Vec<String> {
    let mut lines = vec![
        format!("tool: {}", summary.tool),
        format!("ready: {}", summary.ready),
        format!("changed: {}", summary.changed),
        format!("detail: {}", summary.detail),
    ];
    if let Some(manager) = summary.manager.as_deref() {
        lines.push(format!("manager: {manager}"));
    }
    if let Some(version) = summary.version.as_deref() {
        lines.push(format!("version: {version}"));
    }
    lines
}

fn host_check_row(check: &HostCheckSummary) -> KeyValueRow {
    let value = match (check.version.as_deref(), check.detail.as_deref()) {
        (Some(version), Some(detail)) if !detail.is_empty() => {
            format!("{version} — {detail}")
        }
        (Some(version), None) => version.to_string(),
        (None, Some(detail)) => detail.to_string(),
        (None, None) => check.status.clone(),
        (Some(version), Some(_)) => version.to_string(),
    };

    let tone = match (check.level.as_str(), check.status.as_str()) {
        (_, "ok") => Tone::Success,
        ("required", _) => Tone::Danger,
        ("recommended", _) => Tone::Warning,
        _ => Tone::Muted,
    };

    KeyValueRow::new(check.name.clone(), value, tone)
}
