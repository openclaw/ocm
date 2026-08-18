use std::collections::BTreeMap;

use crate::infra::terminal::{
    Cell, KeyValueRow, Tone, paint, render_key_value_card, render_table, terminal_width,
};
use crate::runtime::{
    RuntimeBinarySummary, RuntimeMeta, RuntimeRelease, RuntimeReleaseSelectorKind,
    RuntimeSourceKind, RuntimeUpdateBatchSummary, RuntimeVerifySummary,
};

use super::{RenderProfile, format_key_value_lines, format_rfc3339};

pub fn runtime_added(
    meta: &RuntimeMeta,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return runtime_added_raw(meta, command_example);
    }

    let mut lines = vec![paint("Runtime added", Tone::Strong, profile.color)];
    push_card(
        &mut lines,
        "Runtime",
        vec![
            KeyValueRow::accent("Name", meta.name.clone()),
            KeyValueRow::plain("Source", source_label(&meta.source_kind)),
            KeyValueRow::accent("Binary", meta.binary_path.clone()),
        ],
        profile.color,
    );
    push_card(
        &mut lines,
        "Next",
        vec![
            KeyValueRow::accent(
                "Use in env",
                format!("{command_example} env create mira --runtime {}", meta.name),
            ),
            KeyValueRow::accent(
                "Show",
                format!("{command_example} runtime show {}", meta.name),
            ),
        ],
        profile.color,
    );
    lines
}

fn runtime_added_raw(meta: &RuntimeMeta, command_example: &str) -> Vec<String> {
    vec![
        format!("Added runtime {}", meta.name),
        format!("  binary path: {}", meta.binary_path),
        format!(
            "  use in env: {command_example} env create mira --runtime {}",
            meta.name
        ),
    ]
}

pub fn runtime_list(runtimes: &[RuntimeMeta], profile: RenderProfile) -> Vec<String> {
    runtime_list_with_width(runtimes, profile, terminal_width())
}

fn runtime_list_with_width(
    runtimes: &[RuntimeMeta],
    profile: RenderProfile,
    width: Option<usize>,
) -> Vec<String> {
    if runtimes.is_empty() {
        return vec![paint("No runtimes.", Tone::Muted, profile.color)];
    }
    if !profile.pretty {
        return runtime_list_raw(runtimes);
    }

    let show_full = width.map(|width| width >= 110).unwrap_or(true);
    let rows = runtimes
        .iter()
        .map(|meta| {
            if show_full {
                vec![
                    Cell::accent(meta.name.clone()),
                    Cell::plain(meta.source_kind.as_str()),
                    optional_cell(selector_summary(meta).as_deref()),
                    optional_cell(meta.release_version.as_deref()),
                    Cell::muted(meta.binary_path.clone()),
                ]
            } else {
                vec![
                    Cell::accent(meta.name.clone()),
                    Cell::plain(meta.source_kind.as_str()),
                    Cell::muted(runtime_target(meta)),
                ]
            }
        })
        .collect::<Vec<_>>();
    let mut lines = render_table(
        if show_full {
            &["Name", "Source", "Tracks", "Current", "Binary"]
        } else {
            &["Name", "Source", "Target"]
        },
        &rows,
        profile.color,
    );
    if !show_full {
        lines.push(String::new());
        lines.push(paint(
            "Use --raw for full runtime path and release details.",
            Tone::Muted,
            profile.color,
        ));
    }
    lines
}

fn runtime_list_raw(runtimes: &[RuntimeMeta]) -> Vec<String> {
    let mut lines = Vec::with_capacity(runtimes.len());
    for meta in runtimes {
        let mut bits = vec![
            meta.name.clone(),
            meta.binary_path.clone(),
            format!("source={}", meta.source_kind.as_str()),
        ];
        if let Some(release_version) = meta.release_version.as_deref() {
            bits.push(format!("release={release_version}"));
        }
        if let Some(release_channel) = meta.release_channel.as_deref() {
            bits.push(format!("channel={release_channel}"));
        }
        if let Some(tracks) = selector_summary(meta) {
            bits.push(format!("tracks={tracks}"));
        }
        lines.push(bits.join("  "));
    }
    lines
}

fn optional_cell(value: Option<&str>) -> Cell {
    value.map(Cell::plain).unwrap_or_else(|| Cell::muted("—"))
}

fn runtime_target(meta: &RuntimeMeta) -> String {
    match selector_summary(meta) {
        Some(selector) => match meta.release_version.as_deref() {
            Some(version)
                if meta.release_selector_kind == Some(RuntimeReleaseSelectorKind::Channel) =>
            {
                format!("{selector} -> {version}")
            }
            _ => selector,
        },
        None => match (
            meta.release_version.as_deref(),
            meta.release_channel.as_deref(),
        ) {
            (Some(version), Some(channel)) => format!("{version} ({channel})"),
            (Some(version), None) => version.to_string(),
            (None, Some(channel)) => format!("channel:{channel}"),
            (None, None) => meta.binary_path.clone(),
        },
    }
}

fn selector_summary(meta: &RuntimeMeta) -> Option<String> {
    match (
        meta.release_selector_kind.as_ref(),
        meta.release_selector_value.as_deref(),
    ) {
        (Some(RuntimeReleaseSelectorKind::Version), Some(value)) => {
            Some(format!("version {value}"))
        }
        (Some(RuntimeReleaseSelectorKind::Channel), Some(value)) => {
            Some(format!("channel {value}"))
        }
        (Some(RuntimeReleaseSelectorKind::Version), None) => Some("version".to_string()),
        (Some(RuntimeReleaseSelectorKind::Channel), None) => Some("channel".to_string()),
        (None, None) => None,
        (None, Some(value)) => Some(value.to_string()),
    }
}

pub fn runtime_show(
    meta: &RuntimeMeta,
    profile: RenderProfile,
    command_example: &str,
) -> Result<Vec<String>, String> {
    if !profile.pretty {
        return runtime_show_raw(meta);
    }

    let mut lines = vec![paint(
        &format!("Runtime {}", meta.name),
        Tone::Strong,
        profile.color,
    )];

    let mut runtime_rows = vec![
        KeyValueRow::accent("Name", meta.name.clone()),
        KeyValueRow::new(
            "Source",
            source_label(&meta.source_kind),
            source_tone(&meta.source_kind),
        ),
        KeyValueRow::accent("Binary", meta.binary_path.clone()),
    ];
    if let Some(description) = meta.description.as_deref() {
        runtime_rows.push(KeyValueRow::plain("Description", description));
    }
    push_card(&mut lines, "Runtime", runtime_rows, profile.color);

    let mut release_rows = Vec::new();
    if let Some(release_version) = meta.release_version.as_deref() {
        release_rows.push(KeyValueRow::accent("Version", release_version));
    }
    if let Some(release_channel) = meta.release_channel.as_deref() {
        release_rows.push(KeyValueRow::plain("Channel", release_channel));
    }
    if let Some(selector_kind) = meta.release_selector_kind.as_ref() {
        release_rows.push(KeyValueRow::plain(
            "Tracks",
            selector_label(selector_kind, meta.release_selector_value.as_deref()),
        ));
    }
    if release_rows.is_empty() {
        release_rows.push(KeyValueRow::muted("Release", "manual"));
    }
    push_card(&mut lines, "Release", release_rows, profile.color);

    let mut source_rows = Vec::new();
    if let Some(install_root) = meta.install_root.as_deref() {
        source_rows.push(KeyValueRow::plain("Install root", install_root));
    }
    if let Some(source_path) = meta.source_path.as_deref() {
        source_rows.push(KeyValueRow::plain("Source path", source_path));
    }
    if let Some(source_url) = meta.source_url.as_deref() {
        source_rows.push(KeyValueRow::plain("Source URL", source_url));
    }
    if let Some(source_manifest_url) = meta.source_manifest_url.as_deref() {
        source_rows.push(KeyValueRow::plain("Manifest URL", source_manifest_url));
    }
    if let Some(source_sha256) = meta.source_sha256.as_deref() {
        let label = if meta.runtime_sha256.is_some() {
            "Launcher SHA-256"
        } else {
            "SHA-256"
        };
        source_rows.push(KeyValueRow::plain(label, source_sha256));
    }
    if let Some(source_integrity) = meta.source_integrity.as_deref() {
        source_rows.push(KeyValueRow::plain("Integrity", source_integrity));
    }
    if let Some(runtime_sha256) = meta.runtime_sha256.as_deref() {
        source_rows.push(KeyValueRow::plain("Runtime SHA-256", runtime_sha256));
    }
    push_card(&mut lines, "Source details", source_rows, profile.color);

    push_card(
        &mut lines,
        "Metadata",
        vec![
            KeyValueRow::muted("Created", format_rfc3339(meta.created_at)?),
            KeyValueRow::muted("Updated", format_rfc3339(meta.updated_at)?),
        ],
        profile.color,
    );

    let next_steps = runtime_show_next_steps(meta, command_example);
    if !next_steps.is_empty() {
        push_card(&mut lines, "Next", next_steps, profile.color);
    }

    Ok(lines)
}

fn runtime_show_next_steps(meta: &RuntimeMeta, command_example: &str) -> Vec<KeyValueRow> {
    let mut rows = vec![
        KeyValueRow::accent(
            "Use in env",
            format!("{command_example} env create mira --runtime {}", meta.name),
        ),
        KeyValueRow::accent(
            "Verify",
            format!("{command_example} runtime verify {}", meta.name),
        ),
    ];
    if meta.release_selector_kind.is_some() {
        rows.push(KeyValueRow::warning(
            "Update",
            format!("{command_example} runtime update {}", meta.name),
        ));
    }
    rows
}

fn runtime_show_raw(meta: &RuntimeMeta) -> Result<Vec<String>, String> {
    let mut lines = BTreeMap::new();
    lines.insert("kind".to_string(), meta.kind.clone());
    lines.insert("name".to_string(), meta.name.clone());
    lines.insert("binaryPath".to_string(), meta.binary_path.clone());
    lines.insert(
        "sourceKind".to_string(),
        meta.source_kind.as_str().to_string(),
    );
    lines.insert("createdAt".to_string(), format_rfc3339(meta.created_at)?);
    lines.insert("updatedAt".to_string(), format_rfc3339(meta.updated_at)?);
    if let Some(description) = meta.description.as_deref() {
        lines.insert("description".to_string(), description.to_string());
    }
    if let Some(source_path) = meta.source_path.as_deref() {
        lines.insert("sourcePath".to_string(), source_path.to_string());
    }
    if let Some(source_url) = meta.source_url.as_deref() {
        lines.insert("sourceUrl".to_string(), source_url.to_string());
    }
    if let Some(source_manifest_url) = meta.source_manifest_url.as_deref() {
        lines.insert(
            "sourceManifestUrl".to_string(),
            source_manifest_url.to_string(),
        );
    }
    if let Some(source_sha256) = meta.source_sha256.as_deref() {
        lines.insert("sourceSha256".to_string(), source_sha256.to_string());
    }
    if let Some(source_integrity) = meta.source_integrity.as_deref() {
        lines.insert("sourceIntegrity".to_string(), source_integrity.to_string());
    }
    if let Some(runtime_sha256) = meta.runtime_sha256.as_deref() {
        lines.insert("runtimeSha256".to_string(), runtime_sha256.to_string());
    }
    if let Some(release_version) = meta.release_version.as_deref() {
        lines.insert("releaseVersion".to_string(), release_version.to_string());
    }
    if let Some(release_channel) = meta.release_channel.as_deref() {
        lines.insert("releaseChannel".to_string(), release_channel.to_string());
    }
    if let Some(release_selector_kind) = meta.release_selector_kind.as_ref() {
        lines.insert(
            "releaseSelectorKind".to_string(),
            release_selector_kind.as_str().to_string(),
        );
    }
    if let Some(release_selector_value) = meta.release_selector_value.as_deref() {
        lines.insert(
            "releaseSelectorValue".to_string(),
            release_selector_value.to_string(),
        );
    }
    if let Some(install_root) = meta.install_root.as_deref() {
        lines.insert("installRoot".to_string(), install_root.to_string());
    }
    Ok(format_key_value_lines(lines))
}

fn push_card(lines: &mut Vec<String>, title: &str, rows: Vec<KeyValueRow>, color: bool) {
    if rows.is_empty() {
        return;
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend(render_key_value_card(title, &rows, color));
}

fn source_label(source_kind: &RuntimeSourceKind) -> &'static str {
    match source_kind {
        RuntimeSourceKind::Registered => "registered",
        RuntimeSourceKind::Installed => "installed",
    }
}

fn source_tone(source_kind: &RuntimeSourceKind) -> Tone {
    match source_kind {
        RuntimeSourceKind::Registered => Tone::Accent,
        RuntimeSourceKind::Installed => Tone::Success,
    }
}

fn selector_label(kind: &RuntimeReleaseSelectorKind, value: Option<&str>) -> String {
    match (kind, value) {
        (RuntimeReleaseSelectorKind::Version, Some(value)) => format!("version {value}"),
        (RuntimeReleaseSelectorKind::Channel, Some(value)) => format!("channel {value}"),
        (RuntimeReleaseSelectorKind::Version, None) => "version".to_string(),
        (RuntimeReleaseSelectorKind::Channel, None) => "channel".to_string(),
    }
}

pub fn runtime_which(
    summary: &RuntimeBinarySummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return vec![summary.binary_path.clone()];
    }

    let mut lines = vec![paint(
        &format!("Runtime path {}", summary.name),
        Tone::Strong,
        profile.color,
    )];
    let mut runtime_rows = vec![
        KeyValueRow::accent("Name", summary.name.clone()),
        KeyValueRow::accent("Binary", summary.binary_path.clone()),
        KeyValueRow::plain("Source", summary.source_kind.clone()),
    ];
    if let Some(release_version) = summary.release_version.as_ref() {
        runtime_rows.push(KeyValueRow::plain(
            "Release version",
            release_version.clone(),
        ));
    }
    if let Some(release_channel) = summary.release_channel.as_ref() {
        runtime_rows.push(KeyValueRow::plain(
            "Release channel",
            release_channel.clone(),
        ));
    }
    if let Some(selector_kind) = summary.release_selector_kind.as_ref() {
        runtime_rows.push(KeyValueRow::plain(
            "Tracks",
            selector_label(selector_kind, summary.release_selector_value.as_deref()),
        ));
    }
    if let Some(install_root) = summary.install_root.as_ref() {
        runtime_rows.push(KeyValueRow::plain("Install root", install_root.clone()));
    }
    push_card(&mut lines, "Runtime", runtime_rows, profile.color);
    push_card(
        &mut lines,
        "Next",
        vec![
            KeyValueRow::accent(
                "Show",
                format!("{command_example} runtime show {}", summary.name),
            ),
            KeyValueRow::accent(
                "Verify",
                format!("{command_example} runtime verify {}", summary.name),
            ),
        ],
        profile.color,
    );
    lines
}

pub fn runtime_removed(name: &str, profile: RenderProfile, command_example: &str) -> Vec<String> {
    if !profile.pretty {
        return vec![format!("Removed runtime {name}")];
    }

    let mut lines = vec![paint("Runtime removed", Tone::Strong, profile.color)];
    push_card(
        &mut lines,
        "Runtime",
        vec![KeyValueRow::accent("Name", name)],
        profile.color,
    );
    push_card(
        &mut lines,
        "Next",
        vec![KeyValueRow::accent(
            "List",
            format!("{command_example} runtime list"),
        )],
        profile.color,
    );
    lines
}

pub fn runtime_installed(
    meta: &RuntimeMeta,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return runtime_installed_raw(meta, command_example);
    }

    runtime_action_receipt("Runtime installed", meta, profile, command_example)
}

pub fn runtime_built(
    meta: &RuntimeMeta,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        let mut lines = runtime_installed_raw(meta, command_example);
        if let Some(first) = lines.first_mut() {
            *first = format!("Built runtime {}", meta.name);
        }
        return lines;
    }

    runtime_action_receipt("Runtime built", meta, profile, command_example)
}

fn runtime_installed_raw(meta: &RuntimeMeta, command_example: &str) -> Vec<String> {
    let mut lines = vec![
        format!("Installed runtime {}", meta.name),
        format!("  binary path: {}", meta.binary_path),
    ];
    if let Some(install_root) = meta.install_root.as_deref() {
        lines.push(format!("  install root: {install_root}"));
    }
    lines.push(format!(
        "  use in env: {command_example} env create mira --runtime {}",
        meta.name
    ));
    lines
}

pub fn runtime_reused(
    meta: &RuntimeMeta,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return runtime_reused_raw(meta, command_example);
    }

    runtime_action_receipt("Runtime ready", meta, profile, command_example)
}

fn runtime_reused_raw(meta: &RuntimeMeta, command_example: &str) -> Vec<String> {
    let mut lines = vec![
        format!("Using installed runtime {}", meta.name),
        format!("  binary path: {}", meta.binary_path),
    ];
    if let Some(install_root) = meta.install_root.as_deref() {
        lines.push(format!("  install root: {install_root}"));
    }
    lines.push(format!(
        "  use in env: {command_example} env create mira --runtime {}",
        meta.name
    ));
    lines
}

pub fn runtime_releases(releases: &[RuntimeRelease]) -> Vec<String> {
    if releases.is_empty() {
        return vec!["No runtime releases.".to_string()];
    }
    let mut lines = Vec::with_capacity(releases.len());
    for release in releases {
        let mut bits = vec![release.version.clone(), release.url.clone()];
        if let Some(channel) = release.channel.as_deref() {
            bits.push(format!("channel={channel}"));
        }
        if let Some(sha256) = release.sha256.as_deref() {
            bits.push(format!("sha256={sha256}"));
        }
        lines.push(bits.join("  "));
    }
    lines
}

pub fn runtime_update_batch(
    batch: &RuntimeUpdateBatchSummary,
    profile: RenderProfile,
) -> Vec<String> {
    if batch.results.is_empty() {
        return if profile.pretty {
            vec![paint("No runtimes.", Tone::Muted, profile.color)]
        } else {
            vec!["No runtimes.".to_string()]
        };
    }
    if !profile.pretty {
        return runtime_update_batch_raw(batch);
    }

    let mut lines = vec![paint(
        "Runtime upgrade summary",
        Tone::Strong,
        profile.color,
    )];
    push_card(
        &mut lines,
        "Summary",
        vec![
            KeyValueRow::plain("Total", batch.count.to_string()),
            KeyValueRow::success("Updated", batch.updated.to_string()),
            KeyValueRow::muted("Skipped", batch.skipped.to_string()),
            KeyValueRow::new(
                "Failed",
                batch.failed.to_string(),
                if batch.failed > 0 {
                    Tone::Danger
                } else {
                    Tone::Muted
                },
            ),
        ],
        profile.color,
    );

    let rows = batch
        .results
        .iter()
        .map(|summary| {
            vec![
                Cell::accent(summary.name.clone()),
                Cell::plain(summary.outcome.clone()),
                Cell::plain(summary.source_kind.clone()),
                summary
                    .release_version
                    .as_deref()
                    .map(Cell::plain)
                    .unwrap_or_else(|| Cell::muted("—")),
            ]
        })
        .collect::<Vec<_>>();
    lines.push(String::new());
    lines.extend(render_table(
        &["Runtime", "Outcome", "Source", "Version"],
        &rows,
        profile.color,
    ));
    lines
}

fn runtime_update_batch_raw(batch: &RuntimeUpdateBatchSummary) -> Vec<String> {
    let mut lines = vec![format!(
        "Runtime update summary: total={} updated={} skipped={} failed={}",
        batch.count, batch.updated, batch.skipped, batch.failed
    )];
    for summary in &batch.results {
        let mut bits = vec![
            summary.name.clone(),
            format!("outcome={}", summary.outcome),
            format!("source={}", summary.source_kind),
        ];
        if let Some(binary_path) = summary.binary_path.as_deref() {
            bits.push(binary_path.to_string());
        }
        if let Some(release_version) = summary.release_version.as_deref() {
            bits.push(format!("release={release_version}"));
        }
        if let Some(release_channel) = summary.release_channel.as_deref() {
            bits.push(format!("channel={release_channel}"));
        }
        if let Some(issue) = summary.issue.as_deref() {
            bits.push(format!("issue={issue}"));
        }
        lines.push(bits.join("  "));
    }
    lines
}

pub fn runtime_updated(
    meta: &RuntimeMeta,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return runtime_updated_raw(meta, command_example);
    }

    runtime_action_receipt("Runtime updated", meta, profile, command_example)
}

fn runtime_updated_raw(meta: &RuntimeMeta, command_example: &str) -> Vec<String> {
    let mut lines = vec![
        format!("Updated runtime {}", meta.name),
        format!("  binary path: {}", meta.binary_path),
    ];
    if let Some(install_root) = meta.install_root.as_deref() {
        lines.push(format!("  install root: {install_root}"));
    }
    lines.push(format!(
        "  use in env: {command_example} env create mira --runtime {}",
        meta.name
    ));
    lines
}

fn runtime_action_receipt(
    title: &str,
    meta: &RuntimeMeta,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    let mut lines = vec![paint(title, Tone::Strong, profile.color)];

    let mut runtime_rows = vec![
        KeyValueRow::accent("Name", meta.name.clone()),
        KeyValueRow::plain("Source", source_label(&meta.source_kind)),
        KeyValueRow::accent("Binary", meta.binary_path.clone()),
    ];
    if let Some(version) = meta.release_version.as_deref() {
        runtime_rows.push(KeyValueRow::plain("Version", version));
    }
    if let Some(selector) = selector_summary(meta) {
        runtime_rows.push(KeyValueRow::plain("Tracks", selector));
    }
    push_card(&mut lines, "Runtime", runtime_rows, profile.color);

    push_card(
        &mut lines,
        "Next",
        vec![
            KeyValueRow::accent(
                "Use in env",
                format!("{command_example} env create mira --runtime {}", meta.name),
            ),
            KeyValueRow::accent(
                "Verify",
                format!("{command_example} runtime verify {}", meta.name),
            ),
        ],
        profile.color,
    );

    lines
}

pub fn runtime_verify_all(
    summaries: &[RuntimeVerifySummary],
    profile: RenderProfile,
) -> Vec<String> {
    if summaries.is_empty() {
        return vec!["No runtimes.".to_string()];
    }
    if !profile.pretty {
        return runtime_verify_all_raw(summaries);
    }

    let show_full = terminal_width().map(|width| width >= 110).unwrap_or(true);
    let rows = summaries
        .iter()
        .map(|summary| {
            let health = if summary.healthy {
                Cell::success("healthy")
            } else {
                Cell::danger("broken")
            };
            if show_full {
                vec![
                    Cell::accent(summary.name.clone()),
                    Cell::plain(summary.source_kind.clone()),
                    health,
                    optional_cell(verify_selector_summary(summary).as_deref()),
                    optional_cell(summary.release_version.as_deref()),
                    optional_cell(summary.issue.as_deref()),
                ]
            } else {
                vec![
                    Cell::accent(summary.name.clone()),
                    health,
                    optional_cell(summary.issue.as_deref()),
                ]
            }
        })
        .collect::<Vec<_>>();

    let mut lines = render_table(
        if show_full {
            &["Name", "Source", "Health", "Tracks", "Current", "Issue"]
        } else {
            &["Name", "Health", "Issue"]
        },
        &rows,
        profile.color,
    );
    if !show_full {
        lines.push(String::new());
        lines.push(paint(
            "Use runtime verify <name>, --raw, or --json for full runtime details.",
            Tone::Muted,
            profile.color,
        ));
    }
    lines
}

fn runtime_verify_all_raw(summaries: &[RuntimeVerifySummary]) -> Vec<String> {
    let mut lines = Vec::with_capacity(summaries.len());
    for summary in summaries {
        let mut bits = vec![
            summary.name.clone(),
            summary.binary_path.clone(),
            format!("source={}", summary.source_kind),
            format!("healthy={}", summary.healthy),
        ];
        if let Some(tracks) = verify_selector_summary(summary) {
            bits.push(format!("tracks={tracks}"));
        }
        if let Some(issue) = summary.issue.as_deref() {
            bits.push(format!("issue={issue}"));
        }
        lines.push(bits.join("  "));
    }
    lines
}

pub fn runtime_verify(
    summary: &RuntimeVerifySummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return runtime_verify_raw(summary);
    }

    let mut lines = vec![paint(
        &format!("Runtime {}", summary.name),
        Tone::Strong,
        profile.color,
    )];

    let mut status_rows = vec![
        KeyValueRow::new(
            "Health",
            if summary.healthy { "healthy" } else { "broken" },
            if summary.healthy {
                Tone::Success
            } else {
                Tone::Danger
            },
        ),
        KeyValueRow::new(
            "Source",
            summary.source_kind.clone(),
            verify_source_tone(summary),
        ),
        KeyValueRow::accent("Binary", summary.binary_path.clone()),
    ];
    if let Some(tracks) = verify_selector_summary(summary) {
        status_rows.push(KeyValueRow::plain("Tracks", tracks));
    }
    if let Some(release_version) = summary.release_version.as_deref() {
        status_rows.push(KeyValueRow::plain("Current", release_version));
    }
    if let Some(issue) = summary.issue.as_deref() {
        status_rows.push(KeyValueRow::danger("Issue", issue));
    }
    push_card(&mut lines, "Verification", status_rows, profile.color);

    let mut source_rows = Vec::new();
    if let Some(install_root) = summary.install_root.as_deref() {
        source_rows.push(KeyValueRow::plain("Install root", install_root));
    }
    if let Some(source_path) = summary.source_path.as_deref() {
        source_rows.push(KeyValueRow::plain("Source path", source_path));
    }
    if let Some(source_url) = summary.source_url.as_deref() {
        source_rows.push(KeyValueRow::plain("Source URL", source_url));
    }
    if let Some(source_manifest_url) = summary.source_manifest_url.as_deref() {
        source_rows.push(KeyValueRow::plain("Manifest URL", source_manifest_url));
    }
    if let Some(source_sha256) = summary.source_sha256.as_deref() {
        let label = if summary.runtime_sha256.is_some() {
            "Launcher SHA-256"
        } else {
            "SHA-256"
        };
        source_rows.push(KeyValueRow::plain(label, source_sha256));
    }
    if let Some(runtime_sha256) = summary.runtime_sha256.as_deref() {
        source_rows.push(KeyValueRow::plain("Runtime SHA-256", runtime_sha256));
    }
    push_card(&mut lines, "Source details", source_rows, profile.color);

    let next_steps = runtime_verify_next_steps(summary, command_example);
    if !next_steps.is_empty() {
        push_card(&mut lines, "Next", next_steps, profile.color);
    }

    lines
}

fn runtime_verify_next_steps(
    summary: &RuntimeVerifySummary,
    command_example: &str,
) -> Vec<KeyValueRow> {
    let mut rows = vec![KeyValueRow::accent(
        "Show",
        format!("{command_example} runtime show {}", summary.name),
    )];

    if summary.healthy {
        rows.push(KeyValueRow::accent(
            "Which",
            format!("{command_example} runtime which {}", summary.name),
        ));
        if summary.release_selector_kind.is_some() {
            rows.push(KeyValueRow::warning(
                "Update",
                format!("{command_example} runtime update {}", summary.name),
            ));
        }
        return rows;
    }

    if summary.release_selector_kind.is_some() {
        rows.push(KeyValueRow::warning(
            "Repair",
            format!("{command_example} runtime update {}", summary.name),
        ));
    } else {
        rows.push(KeyValueRow::warning(
            "Replace",
            format!("{command_example} runtime remove {}", summary.name),
        ));
    }

    rows
}

fn runtime_verify_raw(summary: &RuntimeVerifySummary) -> Vec<String> {
    let mut lines = vec![
        format!("name: {}", summary.name),
        format!("binaryPath: {}", summary.binary_path),
        format!("sourceKind: {}", summary.source_kind),
        format!("healthy: {}", summary.healthy),
    ];
    if let Some(source_path) = summary.source_path.as_deref() {
        lines.push(format!("sourcePath: {source_path}"));
    }
    if let Some(source_url) = summary.source_url.as_deref() {
        lines.push(format!("sourceUrl: {source_url}"));
    }
    if let Some(source_manifest_url) = summary.source_manifest_url.as_deref() {
        lines.push(format!("sourceManifestUrl: {source_manifest_url}"));
    }
    if let Some(source_sha256) = summary.source_sha256.as_deref() {
        lines.push(format!("sourceSha256: {source_sha256}"));
    }
    if let Some(runtime_sha256) = summary.runtime_sha256.as_deref() {
        lines.push(format!("runtimeSha256: {runtime_sha256}"));
    }
    if let Some(release_version) = summary.release_version.as_deref() {
        lines.push(format!("releaseVersion: {release_version}"));
    }
    if let Some(release_channel) = summary.release_channel.as_deref() {
        lines.push(format!("releaseChannel: {release_channel}"));
    }
    if let Some(release_selector_kind) = summary.release_selector_kind.as_ref() {
        lines.push(format!(
            "releaseSelectorKind: {}",
            release_selector_kind.as_str()
        ));
    }
    if let Some(release_selector_value) = summary.release_selector_value.as_deref() {
        lines.push(format!("releaseSelectorValue: {release_selector_value}"));
    }
    if let Some(install_root) = summary.install_root.as_deref() {
        lines.push(format!("installRoot: {install_root}"));
    }
    if let Some(issue) = summary.issue.as_deref() {
        lines.push(format!("issue: {issue}"));
    }
    lines
}

fn verify_selector_summary(summary: &RuntimeVerifySummary) -> Option<String> {
    match (
        summary.release_selector_kind.as_ref(),
        summary.release_selector_value.as_deref(),
    ) {
        (Some(kind), value) => Some(selector_label(kind, value)),
        (None, None) => None,
        (None, Some(value)) => Some(value.to_string()),
    }
}

fn verify_source_tone(summary: &RuntimeVerifySummary) -> Tone {
    match summary.source_kind.as_str() {
        "installed" => Tone::Success,
        "registered" => Tone::Accent,
        _ => Tone::Plain,
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::{
        RenderProfile, runtime_list_with_width, runtime_show, runtime_verify, runtime_verify_all,
        runtime_which,
    };
    use crate::runtime::{
        RuntimeBinarySummary, RuntimeMeta, RuntimeReleaseSelectorKind, RuntimeSourceKind,
        RuntimeVerifySummary,
    };

    #[test]
    fn runtime_list_pretty_compacts_on_narrow_terminals() {
        let lines =
            runtime_list_with_width(&[sample_runtime()], RenderProfile::pretty(false), Some(80));

        assert!(lines[1].contains("Target"));
        assert!(!lines[1].contains("Current"));
        assert!(!lines[1].contains("Binary"));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("channel stable -> 2026.3.24"))
        );
        assert_eq!(
            lines.last().unwrap(),
            "Use --raw for full runtime path and release details."
        );
    }

    #[test]
    fn runtime_list_pretty_keeps_all_columns_on_wide_terminals() {
        let lines =
            runtime_list_with_width(&[sample_runtime()], RenderProfile::pretty(false), Some(140));

        assert!(lines[1].contains("Tracks"));
        assert!(lines[1].contains("Current"));
        assert!(lines[1].contains("Binary"));
        assert!(lines.iter().any(|line| line.contains("/tmp/openclaw")));
        assert!(lines.iter().any(|line| line.contains("channel stable")));
    }

    #[test]
    fn runtime_show_pretty_uses_cards() {
        let lines = runtime_show(&sample_runtime(), RenderProfile::pretty(false), "ocm").unwrap();

        assert_eq!(lines[0], "Runtime stable");
        assert!(lines.iter().any(|line| line.contains("Runtime")));
        assert!(lines.iter().any(|line| line.contains("Release")));
        assert!(lines.iter().any(|line| line.contains("Tracks")));
        assert!(lines.iter().any(|line| line.contains("channel stable")));
        assert!(lines.iter().any(|line| line.contains("Source details")));
        assert!(lines.iter().any(|line| line.contains("Metadata")));
    }

    #[test]
    fn runtime_show_pretty_includes_next_steps() {
        let lines = runtime_show(&sample_runtime(), RenderProfile::pretty(false), "ocm").unwrap();

        assert!(lines.iter().any(|line| line.contains("Next")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm env create mira --runtime stable"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm runtime verify stable"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm runtime update stable"))
        );
    }

    #[test]
    fn runtime_show_raw_keeps_key_value_lines() {
        let lines = runtime_show(&sample_runtime(), RenderProfile::raw(), "ocm").unwrap();

        assert!(lines.iter().any(|line| line == "kind: ocm-runtime"));
        assert!(lines.iter().any(|line| line == "name: stable"));
        assert!(lines.iter().any(|line| line == "sourceKind: installed"));
        assert!(
            lines
                .iter()
                .any(|line| line == "releaseSelectorKind: channel")
        );
        assert!(!lines.iter().any(|line| line.contains('┌')));
    }

    #[test]
    fn runtime_which_pretty_uses_cards() {
        let lines = runtime_which(
            &sample_runtime_binary_summary(),
            RenderProfile::pretty(false),
            "ocm",
        );

        assert_eq!(lines[0], "Runtime path stable");
        assert!(lines.iter().any(|line| line.contains("Runtime")));
        assert!(lines.iter().any(|line| line.contains("Release version")));
        assert!(lines.iter().any(|line| line.contains("Install root")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm runtime verify stable"))
        );
    }

    #[test]
    fn runtime_which_raw_keeps_path_only() {
        let lines = runtime_which(
            &sample_runtime_binary_summary(),
            RenderProfile::raw(),
            "ocm",
        );

        assert_eq!(lines, vec!["/tmp/openclaw".to_string()]);
    }

    #[test]
    fn runtime_verify_pretty_uses_cards() {
        let lines = runtime_verify(
            &sample_verify_summary(),
            RenderProfile::pretty(false),
            "ocm",
        );

        assert_eq!(lines[0], "Runtime stable");
        assert!(lines.iter().any(|line| line.contains("Verification")));
        assert!(lines.iter().any(|line| line.contains("Health")));
        assert!(lines.iter().any(|line| line.contains("Tracks")));
        assert!(lines.iter().any(|line| line.contains("Source details")));
        assert!(lines.iter().any(|line| line.contains("Launcher SHA-256")));
        assert!(lines.iter().any(|line| line.contains("Runtime SHA-256")));
    }

    #[test]
    fn runtime_verify_all_pretty_uses_a_table() {
        let lines = runtime_verify_all(&[sample_verify_summary()], RenderProfile::pretty(false));

        assert!(lines[0].contains("┌"));
        assert!(lines.iter().any(|line| line.contains("Health")));
        assert!(lines.iter().any(|line| line.contains("stable")));
    }

    #[test]
    fn runtime_verify_raw_keeps_key_value_lines() {
        let lines = runtime_verify(&sample_verify_summary(), RenderProfile::raw(), "ocm");

        assert!(lines.iter().any(|line| line == "name: stable"));
        assert!(lines.iter().any(|line| line == "healthy: true"));
        assert!(lines.iter().any(|line| line == "runtimeSha256: def456"));
        assert!(
            lines
                .iter()
                .any(|line| line == "releaseSelectorKind: channel")
        );
        assert!(!lines.iter().any(|line| line.contains('┌')));
    }

    #[test]
    fn runtime_verify_pretty_includes_next_steps() {
        let lines = runtime_verify(
            &sample_verify_summary(),
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(lines.iter().any(|line| line.contains("Next")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm runtime show stable"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm runtime which stable"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm runtime update stable"))
        );
    }

    #[test]
    fn runtime_verify_pretty_prefers_repair_for_broken_selector_backed_runtimes() {
        let mut summary = sample_verify_summary();
        summary.healthy = false;
        summary.issue = Some("sha256 mismatch".to_string());

        let lines = runtime_verify(&summary, RenderProfile::pretty(false), "ocm");

        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm runtime update stable"))
        );
    }

    fn sample_runtime() -> RuntimeMeta {
        RuntimeMeta {
            kind: "ocm-runtime".to_string(),
            name: "stable".to_string(),
            binary_path: "/tmp/openclaw".to_string(),
            source_kind: RuntimeSourceKind::Installed,
            source_path: None,
            source_url: None,
            source_manifest_url: None,
            source_sha256: None,
            source_integrity: None,
            runtime_sha256: None,
            release_version: Some("2026.3.24".to_string()),
            release_channel: Some("stable".to_string()),
            release_selector_kind: Some(RuntimeReleaseSelectorKind::Channel),
            release_selector_value: Some("stable".to_string()),
            install_root: Some("/tmp/ocm/runtimes/stable".to_string()),
            description: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn sample_verify_summary() -> RuntimeVerifySummary {
        RuntimeVerifySummary {
            name: "stable".to_string(),
            binary_path: "/tmp/openclaw".to_string(),
            source_kind: "installed".to_string(),
            source_path: None,
            source_url: Some(
                "https://registry.npmjs.org/openclaw/-/openclaw-2026.3.24.tgz".to_string(),
            ),
            source_manifest_url: Some("https://registry.npmjs.org/openclaw".to_string()),
            source_sha256: Some("abc123".to_string()),
            source_integrity: Some("sha512-abc123".to_string()),
            runtime_sha256: Some("def456".to_string()),
            release_version: Some("2026.3.24".to_string()),
            release_channel: Some("stable".to_string()),
            release_selector_kind: Some(RuntimeReleaseSelectorKind::Channel),
            release_selector_value: Some("stable".to_string()),
            install_root: Some("/tmp/ocm/runtimes/stable".to_string()),
            healthy: true,
            issue: None,
        }
    }

    fn sample_runtime_binary_summary() -> RuntimeBinarySummary {
        RuntimeBinarySummary {
            name: "stable".to_string(),
            binary_path: "/tmp/openclaw".to_string(),
            source_kind: "installed".to_string(),
            release_version: Some("2026.3.24".to_string()),
            release_channel: Some("stable".to_string()),
            release_selector_kind: Some(RuntimeReleaseSelectorKind::Channel),
            release_selector_value: Some("stable".to_string()),
            install_root: Some("/tmp/ocm/runtimes/stable".to_string()),
        }
    }
}
