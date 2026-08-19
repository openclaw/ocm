use crate::cli::upgrade::{
    UpgradeBatchSummary, UpgradeEnvSummary, UpgradeRollbackSummary, UpgradeSimulationBatchSummary,
    UpgradeSimulationSummary,
};
use crate::infra::terminal::{
    Cell, KeyValueRow, Tone, paint, render_key_value_card, render_table, terminal_width,
};
use crate::store::UpgradeHistoryRecord;

use super::{RenderProfile, format_rfc3339};

pub fn upgrade_history(
    env_name: &str,
    records: &[UpgradeHistoryRecord],
    profile: RenderProfile,
) -> Result<Vec<String>, String> {
    if !profile.pretty {
        return records
            .iter()
            .map(upgrade_history_raw_line)
            .collect::<Result<Vec<_>, _>>();
    }
    if records.is_empty() {
        return Ok(vec![paint(
            &format!("No upgrade history for {env_name}."),
            Tone::Muted,
            profile.color,
        )]);
    }

    let rows = records
        .iter()
        .map(|record| {
            Ok(vec![
                Cell::accent(record.id.clone()),
                Cell::plain(format!("{}:{}", record.source.kind, record.source.name)),
                Cell::plain(format!("{}:{}", record.target.kind, record.target.name)),
                Cell::new(
                    record.outcome.clone(),
                    crate::infra::terminal::Align::Left,
                    outcome_tone(&record.outcome),
                ),
                Cell::plain(format_rfc3339(record.completed_at)?),
            ])
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(render_table(
        &["Transaction", "From", "To", "Outcome", "Completed"],
        &rows,
        profile.color,
    ))
}

pub fn upgrade_env(
    summary: &UpgradeEnvSummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return upgrade_env_raw(summary);
    }

    let mut lines = vec![paint(
        &format!("Upgrade {}", summary.env_name),
        Tone::Strong,
        profile.color,
    )];
    lines.push(String::new());
    lines.extend(render_key_value_card(
        "Result",
        &[
            KeyValueRow::accent("Env", &summary.env_name),
            KeyValueRow::plain(
                "Was using",
                format!(
                    "{}:{}",
                    summary.previous_binding_kind, summary.previous_binding_name
                ),
            ),
            KeyValueRow::plain(
                "Now using",
                format!("{}:{}", summary.binding_kind, summary.binding_name),
            ),
            KeyValueRow::new("OpenClaw", &summary.outcome, outcome_tone(&summary.outcome)),
            KeyValueRow::new(
                "Service",
                summary.service_action.as_deref().unwrap_or("unchanged"),
                service_tone(summary.service_action.as_deref()),
            ),
            KeyValueRow::plain(
                "Snapshot",
                summary.snapshot_id.as_deref().unwrap_or("not created"),
            ),
            KeyValueRow::new(
                "Rollback",
                summary.rollback.as_deref().unwrap_or("not needed"),
                rollback_tone(summary.rollback.as_deref()),
            ),
        ],
        profile.color,
    ));

    let mut release_rows = Vec::new();
    if let Some(version) = summary.runtime_release_version.as_deref() {
        release_rows.push(KeyValueRow::accent("Version", version));
    }
    if let Some(channel) = summary.runtime_release_channel.as_deref() {
        release_rows.push(KeyValueRow::plain("Channel", channel));
    }
    if !release_rows.is_empty() {
        lines.push(String::new());
        lines.extend(render_key_value_card(
            "Release",
            &release_rows,
            profile.color,
        ));
    }

    if let Some(note) = summary.note.as_deref() {
        lines.push(String::new());
        lines.extend(render_key_value_card(
            "Next",
            &[KeyValueRow::muted("Note", note)],
            profile.color,
        ));
    } else if matches!(
        summary.outcome.as_str(),
        "pinned" | "local-command" | "manual-runtime"
    ) {
        lines.push(String::new());
        lines.extend(render_key_value_card(
            "Next",
            &[KeyValueRow::muted(
                "Hint",
                format!("{command_example} help upgrade"),
            )],
            profile.color,
        ));
    }

    lines
}

pub fn upgrade_rollback(summary: &UpgradeRollbackSummary, profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        return upgrade_rollback_raw(summary);
    }

    let mut lines = vec![paint(
        &format!("Upgrade Rollback {}", summary.env_name),
        Tone::Strong,
        profile.color,
    )];
    lines.push(String::new());
    lines.extend(render_key_value_card(
        "Result",
        &[
            KeyValueRow::accent("Env", &summary.env_name),
            KeyValueRow::plain(
                "Was using",
                format!(
                    "{}:{}",
                    summary.previous_binding_kind, summary.previous_binding_name
                ),
            ),
            KeyValueRow::plain(
                "Now using",
                format!("{}:{}", summary.binding_kind, summary.binding_name),
            ),
            KeyValueRow::new("Outcome", &summary.outcome, outcome_tone(&summary.outcome)),
            KeyValueRow::new(
                "Service",
                summary.service_action.as_deref().unwrap_or("unchanged"),
                service_tone(summary.service_action.as_deref()),
            ),
            KeyValueRow::accent("Restored snapshot", &summary.restored_snapshot_id),
            KeyValueRow::plain("Safety snapshot", rollback_safety_snapshot_display(summary)),
        ],
        profile.color,
    ));
    lines.push(String::new());
    lines.extend(render_key_value_card(
        "History",
        &[
            KeyValueRow::plain("Rolled back", &summary.transaction_id),
            KeyValueRow::plain(
                "Transaction",
                summary
                    .rollback_transaction_id
                    .as_deref()
                    .unwrap_or("not recorded"),
            ),
        ],
        profile.color,
    ));
    if let Some(version) = summary.runtime_release_version.as_deref() {
        lines.push(String::new());
        lines.extend(render_key_value_card(
            "Release",
            &[KeyValueRow::accent("OpenClaw", version)],
            profile.color,
        ));
    }
    if let Some(note) = summary.note.as_deref() {
        lines.push(String::new());
        lines.extend(render_key_value_card(
            "Details",
            &[KeyValueRow::muted("Note", note)],
            profile.color,
        ));
    }
    lines
}

pub fn upgrade_batch(
    summary: &UpgradeBatchSummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return upgrade_batch_raw(summary);
    }

    if summary.results.is_empty() {
        return vec![paint("No environments.", Tone::Muted, profile.color)];
    }

    let wide = terminal_width().map(|width| width >= 110).unwrap_or(true);
    let rows = summary
        .results
        .iter()
        .map(|result| {
            let mut row = vec![
                Cell::accent(result.env_name.clone()),
                Cell::plain(format!("{}:{}", result.binding_kind, result.binding_name)),
                Cell::new(
                    result.outcome.clone(),
                    crate::infra::terminal::Align::Left,
                    outcome_tone(&result.outcome),
                ),
                Cell::new(
                    result
                        .service_action
                        .clone()
                        .unwrap_or_else(|| "unchanged".to_string()),
                    crate::infra::terminal::Align::Left,
                    service_tone(result.service_action.as_deref()),
                ),
            ];
            if wide {
                row.push(
                    result
                        .runtime_release_version
                        .as_deref()
                        .map(Cell::plain)
                        .unwrap_or_else(|| Cell::muted("—")),
                );
                row.push(
                    result
                        .note
                        .as_deref()
                        .map(Cell::muted)
                        .unwrap_or_else(|| Cell::muted("—")),
                );
            }
            row
        })
        .collect::<Vec<_>>();

    let mut lines = render_table(
        if wide {
            &["Env", "Using", "OpenClaw", "Service", "Version", "Notes"]
        } else {
            &["Env", "Using", "OpenClaw", "Service"]
        },
        &rows,
        profile.color,
    );
    lines.push(String::new());
    lines.extend(render_key_value_card(
        "Summary",
        &[
            KeyValueRow::accent("Checked", summary.count.to_string()),
            KeyValueRow::success("Changed", summary.changed.to_string()),
            KeyValueRow::plain("Current", summary.current.to_string()),
            KeyValueRow::warning("Skipped", summary.skipped.to_string()),
            KeyValueRow::plain("Services", summary.restarted.to_string()),
            KeyValueRow::danger("Failed", summary.failed.to_string()),
        ],
        profile.color,
    ));
    if !wide {
        lines.push(String::new());
        lines.push(paint(
            &format!("Use {command_example} upgrade <env> or --raw for notes."),
            Tone::Muted,
            profile.color,
        ));
    }
    lines
}

pub fn upgrade_simulation(
    summary: &UpgradeSimulationSummary,
    profile: RenderProfile,
    _command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return upgrade_simulation_raw(summary);
    }

    let mut lines = vec![paint(
        &format!("Upgrade Simulation {}", summary.source_env),
        Tone::Strong,
        profile.color,
    )];
    lines.push(String::new());
    lines.extend(render_key_value_card(
        "Target",
        &[
            KeyValueRow::accent("Source env", &summary.source_env),
            KeyValueRow::plain("Scenario", &summary.scenario),
            KeyValueRow::plain("Temp env", simulation_temp_label(summary)),
            KeyValueRow::plain(
                "From",
                format!(
                    "{}:{}",
                    summary.from_binding_kind, summary.from_binding_name
                ),
            ),
            KeyValueRow::plain(
                "To",
                format!("{}:{}", summary.to_binding_kind, summary.to_binding_name),
            ),
            KeyValueRow::new(
                "Result",
                &summary.outcome,
                simulation_tone(&summary.outcome),
            ),
            KeyValueRow::new(
                "Cleanup",
                &summary.cleanup,
                simulation_cleanup_tone(&summary.cleanup),
            ),
        ],
        profile.color,
    ));

    let rows = summary
        .checks
        .iter()
        .filter(|check| check.name != "clone env")
        .map(|check| {
            vec![
                Cell::plain(check.name.clone()),
                Cell::new(
                    check.status.clone(),
                    crate::infra::terminal::Align::Left,
                    simulation_tone(&check.status),
                ),
                check
                    .note
                    .as_deref()
                    .map(|note| {
                        Cell::new(
                            note,
                            crate::infra::terminal::Align::Left,
                            if check.status == "failed" {
                                Tone::Danger
                            } else {
                                Tone::Muted
                            },
                        )
                    })
                    .unwrap_or_else(|| Cell::muted("—")),
            ]
        })
        .collect::<Vec<_>>();
    lines.push(String::new());
    lines.extend(render_table(
        &["Check", "Status", "Note"],
        &rows,
        profile.color,
    ));

    if let Some(note) = summary.note.as_deref() {
        lines.push(paint(note, Tone::Muted, profile.color));
    }

    lines
}

pub fn upgrade_simulation_batch(
    summary: &UpgradeSimulationBatchSummary,
    profile: RenderProfile,
    _command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return upgrade_simulation_batch_raw(summary);
    }

    let mut lines = vec![paint(
        &format!("Upgrade Simulations {}", summary.source_env),
        Tone::Strong,
        profile.color,
    )];
    lines.push(String::new());
    lines.extend(render_key_value_card(
        "Summary",
        &[
            KeyValueRow::accent("Source env", &summary.source_env),
            KeyValueRow::plain("Target", &summary.to),
            KeyValueRow::plain("Scenarios", summary.count.to_string()),
            KeyValueRow::new(
                "Result",
                format!("{} passed, {} failed", summary.passed, summary.failed),
                if summary.failed == 0 {
                    Tone::Success
                } else {
                    Tone::Danger
                },
            ),
        ],
        profile.color,
    ));

    let rows = summary
        .results
        .iter()
        .map(|result| {
            let failed_check = result
                .checks
                .iter()
                .find(|check| check.status == "failed")
                .map(|check| check.name.as_str())
                .unwrap_or("—");
            vec![
                Cell::plain(result.scenario.clone()),
                Cell::new(
                    result.outcome.clone(),
                    crate::infra::terminal::Align::Left,
                    simulation_tone(&result.outcome),
                ),
                Cell::new(
                    result.cleanup.clone(),
                    crate::infra::terminal::Align::Left,
                    simulation_cleanup_tone(&result.cleanup),
                ),
                Cell::plain(failed_check),
            ]
        })
        .collect::<Vec<_>>();
    lines.push(String::new());
    lines.extend(render_table(
        &["Scenario", "Result", "Cleanup", "Failed check"],
        &rows,
        profile.color,
    ));

    let failure_rows = summary
        .results
        .iter()
        .flat_map(|result| {
            result
                .checks
                .iter()
                .filter(|check| check.status == "failed")
                .map(|check| {
                    vec![
                        Cell::plain(result.scenario.clone()),
                        Cell::plain(check.name.clone()),
                        Cell::new(
                            check.note.as_deref().unwrap_or("no details reported"),
                            crate::infra::terminal::Align::Left,
                            Tone::Danger,
                        ),
                    ]
                })
        })
        .collect::<Vec<_>>();
    if !failure_rows.is_empty() {
        lines.push(String::new());
        lines.push(paint("Failures", Tone::Danger, profile.color));
        lines.push(String::new());
        lines.extend(render_table(
            &["Scenario", "Check", "Error"],
            &failure_rows,
            profile.color,
        ));
    }

    lines
}

fn upgrade_env_raw(summary: &UpgradeEnvSummary) -> Vec<String> {
    let mut bits = vec![
        summary.env_name.clone(),
        format!(
            "from={}:{}",
            summary.previous_binding_kind, summary.previous_binding_name
        ),
        format!("to={}:{}", summary.binding_kind, summary.binding_name),
        format!("outcome={}", summary.outcome),
    ];
    if let Some(action) = summary.service_action.as_deref() {
        bits.push(format!("service={action}"));
    }
    if let Some(snapshot_id) = summary.snapshot_id.as_deref() {
        bits.push(format!("snapshot={snapshot_id}"));
    }
    if let Some(rollback) = summary.rollback.as_deref() {
        bits.push(format!("rollback={rollback}"));
    }
    if let Some(version) = summary.runtime_release_version.as_deref() {
        bits.push(format!("version={version}"));
    }
    if let Some(channel) = summary.runtime_release_channel.as_deref() {
        bits.push(format!("channel={channel}"));
    }
    if let Some(note) = summary.note.as_deref() {
        bits.push(format!("note={note}"));
    }
    vec![bits.join("  ")]
}

fn upgrade_rollback_raw(summary: &UpgradeRollbackSummary) -> Vec<String> {
    let mut bits = vec![
        format!("env={}", summary.env_name),
        format!("transaction={}", summary.transaction_id),
        format!(
            "from={}:{}",
            summary.previous_binding_kind, summary.previous_binding_name
        ),
        format!("to={}:{}", summary.binding_kind, summary.binding_name),
        format!("outcome={}", summary.outcome),
        format!("restoredSnapshot={}", summary.restored_snapshot_id),
    ];
    if let Some(transaction_id) = summary.rollback_transaction_id.as_deref() {
        bits.push(format!("rollbackTransaction={transaction_id}"));
    }
    if let Some(snapshot_id) = summary.safety_snapshot_id.as_deref() {
        bits.push(format!("safetySnapshot={snapshot_id}"));
    }
    if let Some(action) = summary.service_action.as_deref() {
        bits.push(format!("service={action}"));
    }
    if let Some(version) = summary.runtime_release_version.as_deref() {
        bits.push(format!("version={version}"));
    }
    if let Some(note) = summary.note.as_deref() {
        bits.push(format!("note={note}"));
    }
    vec![bits.join("  ")]
}

fn rollback_safety_snapshot_display(summary: &UpgradeRollbackSummary) -> &str {
    if let Some(snapshot_id) = summary.safety_snapshot_id.as_deref() {
        snapshot_id
    } else if summary.outcome == "would-rollback" {
        "not created"
    } else {
        "removed"
    }
}

fn upgrade_history_raw_line(record: &UpgradeHistoryRecord) -> Result<String, String> {
    let mut bits = vec![
        format!("id={}", record.id),
        format!("env={}", record.env_name),
        format!("from={}:{}", record.source.kind, record.source.name),
        format!("to={}:{}", record.target.kind, record.target.name),
        format!("outcome={}", record.outcome),
        format!("snapshot={}", record.snapshot_id),
        format!("started={}", format_rfc3339(record.started_at)?),
        format!("completed={}", format_rfc3339(record.completed_at)?),
    ];
    if let Some(version) = record.source.openclaw_version.as_deref() {
        bits.push(format!("fromVersion={version}"));
    }
    if let Some(version) = record.target.openclaw_version.as_deref() {
        bits.push(format!("toVersion={version}"));
    }
    if let Some(rollback) = record.rollback.as_deref() {
        bits.push(format!("rollback={rollback}"));
    }
    if let Some(rollback_of) = record.rollback_of.as_deref() {
        bits.push(format!("rollbackOf={rollback_of}"));
    }
    if !record.phases.is_empty() {
        bits.push(format!(
            "phases={}",
            record
                .phases
                .iter()
                .map(|phase| format!(
                    "{}:{}:{}:{}+{}:{}",
                    phase.owner,
                    phase.phase,
                    phase.window,
                    phase.started_offset_ms,
                    phase.duration_ms,
                    phase.outcome
                ))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    Ok(bits.join("  "))
}

fn upgrade_batch_raw(summary: &UpgradeBatchSummary) -> Vec<String> {
    let mut lines = vec![format!(
        "checked={}  changed={}  current={}  skipped={}  restarted={}  failed={}",
        summary.count,
        summary.changed,
        summary.current,
        summary.skipped,
        summary.restarted,
        summary.failed
    )];
    for result in &summary.results {
        lines.extend(upgrade_env_raw(result));
    }
    lines
}

fn upgrade_simulation_raw(summary: &UpgradeSimulationSummary) -> Vec<String> {
    let mut lines = vec![format!(
        "scenario={}  source={}  simulation={}  from={}:{}  to={}:{}  outcome={}  cleanup={}  target={}",
        summary.scenario,
        summary.source_env,
        summary.simulation_env,
        summary.from_binding_kind,
        summary.from_binding_name,
        summary.to_binding_kind,
        summary.to_binding_name,
        summary.outcome,
        summary.cleanup,
        summary.to
    )];
    for check in &summary.checks {
        let mut line = format!("check={}  status={}", check.name, check.status);
        if let Some(note) = check.note.as_deref() {
            line.push_str(&format!("  note={note}"));
        }
        lines.push(line);
    }
    if summary.cleanup != "cleaned" {
        lines.push(format!("cleanup={}", summary.cleanup_command));
    }
    if let Some(note) = summary.note.as_deref() {
        lines.push(format!("note={note}"));
    }
    lines
}

fn upgrade_simulation_batch_raw(summary: &UpgradeSimulationBatchSummary) -> Vec<String> {
    let mut lines = vec![format!(
        "source={}  target={}  scenarios={}  passed={}  failed={}",
        summary.source_env, summary.to, summary.count, summary.passed, summary.failed
    )];
    for result in &summary.results {
        lines.extend(upgrade_simulation_raw(result));
    }
    lines
}

fn outcome_tone(outcome: &str) -> Tone {
    match outcome {
        "updated" | "switched" => Tone::Success,
        "would-update" | "would-switch" => Tone::Accent,
        "up-to-date" => Tone::Accent,
        "pinned" | "local-command" | "manual-runtime" => Tone::Warning,
        "rolled-back" | "rollback-failed" | "failed" => Tone::Danger,
        _ => Tone::Plain,
    }
}

fn service_tone(action: Option<&str>) -> Tone {
    match action {
        Some("restarted") | Some("reloaded") => Tone::Success,
        Some("would-restart") | Some("would-start") => Tone::Accent,
        Some(_) => Tone::Warning,
        None => Tone::Muted,
    }
}

fn rollback_tone(action: Option<&str>) -> Tone {
    match action {
        Some("restored") => Tone::Warning,
        Some("failed") => Tone::Danger,
        Some("disabled") => Tone::Warning,
        Some(_) => Tone::Plain,
        None => Tone::Muted,
    }
}

fn simulation_tone(value: &str) -> Tone {
    match value {
        "passed" => Tone::Success,
        "failed" => Tone::Danger,
        "skipped" => Tone::Muted,
        _ => Tone::Plain,
    }
}

fn simulation_cleanup_tone(value: &str) -> Tone {
    match value {
        "cleaned" => Tone::Success,
        "kept" => Tone::Warning,
        "failed" => Tone::Danger,
        _ => Tone::Muted,
    }
}

fn simulation_temp_label(summary: &UpgradeSimulationSummary) -> String {
    match summary.cleanup.as_str() {
        "cleaned" => "cleaned".to_string(),
        "kept" => summary.simulation_env.clone(),
        "failed" => format!("cleanup failed for {}", summary.simulation_env),
        _ => summary.simulation_env.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::upgrade::{
        UpgradeRollbackSummary, UpgradeSimulationBatchSummary, UpgradeSimulationCheck,
    };

    fn rollback_summary(outcome: &str) -> UpgradeRollbackSummary {
        UpgradeRollbackSummary {
            env_name: "demo".to_string(),
            transaction_id: "upgrade-1".to_string(),
            rollback_transaction_id: Some("rollback-1".to_string()),
            previous_binding_kind: "runtime".to_string(),
            previous_binding_name: "new".to_string(),
            binding_kind: "runtime".to_string(),
            binding_name: "old".to_string(),
            outcome: outcome.to_string(),
            runtime_release_version: Some("2026.6.11".to_string()),
            service_action: None,
            restored_snapshot_id: "pre-upgrade".to_string(),
            safety_snapshot_id: None,
            note: None,
        }
    }

    #[test]
    fn rollback_pretty_distinguishes_removed_and_uncreated_safety_snapshots() {
        let removed =
            upgrade_rollback(&rollback_summary("failed"), RenderProfile::pretty(false)).join("\n");
        let dry_run = upgrade_rollback(
            &rollback_summary("would-rollback"),
            RenderProfile::pretty(false),
        )
        .join("\n");

        assert!(removed.contains("removed"), "{removed}");
        assert!(!removed.contains("not created"), "{removed}");
        assert!(dry_run.contains("not created"), "{dry_run}");
        assert!(!dry_run.contains("removed"), "{dry_run}");
    }

    #[test]
    fn upgrade_simulation_batch_pretty_surfaces_failure_details() {
        let summary = UpgradeSimulationBatchSummary {
            source_env: "demo".to_string(),
            to: "2026.4.23".to_string(),
            count: 1,
            passed: 0,
            failed: 1,
            results: vec![UpgradeSimulationSummary {
                scenario: "current".to_string(),
                source_env: "demo".to_string(),
                simulation_env: "demo-current-sim-1".to_string(),
                from_binding_kind: "runtime".to_string(),
                from_binding_name: "2026.4.20".to_string(),
                to_binding_kind: "unknown".to_string(),
                to_binding_name: "unknown".to_string(),
                to: "2026.4.23".to_string(),
                outcome: "failed".to_string(),
                checks: vec![UpgradeSimulationCheck {
                    name: "prepare target".to_string(),
                    status: "failed".to_string(),
                    note: Some("release 2026.4.23 was not found".to_string()),
                }],
                cleanup_command: "./bin/ocm env destroy demo-current-sim-1 --yes".to_string(),
                cleanup: "cleaned".to_string(),
                note: None,
            }],
        };

        let output = upgrade_simulation_batch(&summary, RenderProfile::pretty(false), "./bin/ocm")
            .join("\n");

        assert!(output.contains("Failures"), "{output}");
        assert!(output.contains("prepare target"), "{output}");
        assert!(output.contains("cleaned"), "{output}");
        assert!(!output.contains("Next"), "{output}");
        assert!(
            output.contains("release 2026.4.23 was not found"),
            "{output}"
        );
    }
}
