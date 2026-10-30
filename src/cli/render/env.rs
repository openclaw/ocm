use std::collections::BTreeMap;

use crate::cli::env::{BlockedPruneSummary, EnvDestroySummary};
use crate::env::{
    EnvCleanupActionSummary, EnvCleanupBatchSummary, EnvCleanupSummary, EnvDoctorSummary,
    EnvExportSummary, EnvImportSummary, EnvSnapshotRemoveSummary, EnvSnapshotRestoreSummary,
    EnvSnapshotSummary, EnvStatusSummary, EnvSummary, ExecutionSummary,
};
use crate::infra::terminal::{
    Cell, KeyValueRow, Tone, paint, render_key_value_card, render_table, render_table_with_limit,
    render_tags, terminal_width,
};

use super::{RenderProfile, format_key_value_lines, format_rfc3339};

pub fn env_protected(name: &str, protected: bool, profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        return vec![format!("Updated env {name}: protected={protected}")];
    }

    let mut lines = vec![paint("Environment updated", Tone::Strong, profile.color)];
    push_card(
        &mut lines,
        "Environment",
        vec![
            KeyValueRow::accent("Name", name),
            KeyValueRow::new(
                "Protected",
                if protected { "yes" } else { "no" },
                if protected {
                    Tone::Warning
                } else {
                    Tone::Muted
                },
            ),
        ],
        profile.color,
    );
    lines
}

pub fn env_removed(name: &str, root: &str, profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        return vec![format!("Removed env {name}"), format!("  root: {root}")];
    }

    let mut lines = vec![paint("Environment removed", Tone::Strong, profile.color)];
    push_card(
        &mut lines,
        "Environment",
        vec![
            KeyValueRow::accent("Name", name),
            KeyValueRow::plain("Root", root),
        ],
        profile.color,
    );
    lines
}

pub fn env_destroy_preview(
    summary: &EnvDestroySummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return env_destroy_preview_raw(summary, command_example);
    }

    let mut lines = vec![paint(
        &format!("Destroy env {}", summary.env_name),
        Tone::Strong,
        profile.color,
    )];

    push_card(
        &mut lines,
        "Summary",
        vec![
            KeyValueRow::plain("Root", summary.root.clone()),
            optional_value_row("Worktree", summary.dev_worktree.clone()),
            KeyValueRow::new(
                "Protected",
                if summary.protected { "yes" } else { "no" },
                if summary.protected {
                    Tone::Warning
                } else {
                    Tone::Muted
                },
            ),
            KeyValueRow::plain("Snapshots", summary.snapshot_count.to_string()),
            KeyValueRow::new(
                "OCM service",
                if summary.service_installed || summary.service_loaded || summary.service_running {
                    "present"
                } else {
                    "absent"
                },
                if summary.service_installed || summary.service_loaded || summary.service_running {
                    Tone::Warning
                } else {
                    Tone::Muted
                },
            ),
        ],
        profile.color,
    );

    let plan_rows = summary
        .steps
        .iter()
        .map(|step| KeyValueRow::plain(&step.kind, step.description.clone()))
        .collect::<Vec<_>>();
    push_card(&mut lines, "Plan", plan_rows, profile.color);

    if !summary.blockers.is_empty() {
        let blocker_rows = summary
            .blockers
            .iter()
            .enumerate()
            .map(|(index, blocker)| KeyValueRow::danger(format!("#{}", index + 1), blocker.clone()))
            .collect::<Vec<_>>();
        push_card(&mut lines, "Blocked", blocker_rows, profile.color);
    } else {
        push_card(
            &mut lines,
            "Apply",
            vec![KeyValueRow::warning(
                "Run",
                format!("{command_example} env destroy {} --yes", summary.env_name),
            )],
            profile.color,
        );
    }

    lines
}

fn env_destroy_preview_raw(summary: &EnvDestroySummary, command_example: &str) -> Vec<String> {
    let mut lines = vec![format!("Destroy preview for env {}", summary.env_name)];
    lines.push(format!("  root: {}", summary.root));
    if let Some(worktree) = &summary.dev_worktree {
        lines.push(format!("  worktree: {worktree}"));
    }
    if summary.snapshot_count > 0 {
        lines.push(format!("  snapshots: {}", summary.snapshot_count));
    }
    if summary.service_installed || summary.service_loaded || summary.service_running {
        lines.push(format!("  service: {}", summary.service_label));
    }

    for step in &summary.steps {
        lines.push(format!("  {}: {}", step.kind, step.description));
    }

    if !summary.blockers.is_empty() {
        lines.push("  blocked:".to_string());
        for blocker in &summary.blockers {
            lines.push(format!("    {blocker}"));
        }
    } else {
        lines.push(format!(
            "  re-run with --yes to destroy it: {command_example} env destroy {} --yes",
            summary.env_name
        ));
    }

    lines
}

pub fn env_destroyed(
    summary: &EnvDestroySummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return env_destroyed_raw(summary, command_example);
    }

    let mut lines = vec![paint(
        &format!("Destroyed env {}", summary.env_name),
        Tone::Strong,
        profile.color,
    )];

    push_card(
        &mut lines,
        "Removed",
        vec![
            KeyValueRow::plain("Root", summary.root.clone()),
            KeyValueRow::plain("Snapshots", summary.snapshots_removed.to_string()),
            KeyValueRow::plain("Processes", summary.processes_terminated.to_string()),
            KeyValueRow::plain(
                "Source watch",
                if summary.source_watch_stopped {
                    "stopped"
                } else {
                    "none"
                },
            ),
            KeyValueRow::new(
                "Worktree",
                if summary.dev_worktree.is_some() {
                    if summary.worktree_removed {
                        "removed"
                    } else {
                        "kept"
                    }
                } else {
                    "none"
                },
                if summary.worktree_removed {
                    Tone::Warning
                } else {
                    Tone::Muted
                },
            ),
            KeyValueRow::new(
                "Environment service",
                if summary.service_uninstalled {
                    "uninstalled"
                } else {
                    "none"
                },
                if summary.service_uninstalled {
                    Tone::Warning
                } else {
                    Tone::Muted
                },
            ),
        ],
        profile.color,
    );

    if !summary.warnings.is_empty() {
        let rows = summary
            .warnings
            .iter()
            .map(|warning| KeyValueRow::warning("Warning", warning.clone()))
            .collect::<Vec<_>>();
        lines.extend(render_key_value_card("Warnings", &rows, profile.color));
    }

    push_card(
        &mut lines,
        "Next",
        vec![
            KeyValueRow::accent("List envs", format!("{command_example} env list")),
            KeyValueRow::accent(
                "Start again",
                format!("{command_example} start {}", summary.env_name),
            ),
        ],
        profile.color,
    );

    lines
}

fn env_destroyed_raw(summary: &EnvDestroySummary, command_example: &str) -> Vec<String> {
    let mut lines = vec![format!("Destroyed env {}", summary.env_name)];
    lines.push(format!("  root: {}", summary.root));
    if summary.snapshots_removed > 0 {
        lines.push(format!(
            "  snapshots removed: {}",
            summary.snapshots_removed
        ));
    }
    if summary.source_watch_stopped {
        lines.push("  source watch stopped".to_string());
    }
    if summary.processes_terminated > 0 {
        lines.push(format!(
            "  processes terminated: {}",
            summary.processes_terminated
        ));
    }
    if summary.worktree_removed {
        lines.push("  worktree removed".to_string());
    }
    if summary.service_uninstalled {
        lines.push(format!(
            "  environment service uninstalled: {}",
            summary.env_name
        ));
    }
    lines.extend(
        summary
            .warnings
            .iter()
            .map(|warning| format!("warning: {warning}")),
    );
    lines.push(format!("  list: {command_example} env list"));
    lines
}

pub fn env_prune_preview(
    older_than_days: i64,
    candidates: &[EnvSummary],
    blocked: &[BlockedPruneSummary],
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return env_prune_preview_raw(older_than_days, candidates, blocked);
    }

    let mut lines = vec![paint(
        "Environment prune preview",
        Tone::Strong,
        profile.color,
    )];
    let mut summary = vec![
        KeyValueRow::plain("Older than", format!("{older_than_days} day(s)")),
        KeyValueRow::warning("Candidates", candidates.len().to_string()),
    ];
    if !blocked.is_empty() {
        summary.push(KeyValueRow::danger("Blocked", blocked.len().to_string()));
    }
    push_card(&mut lines, "Summary", summary, profile.color);
    if candidates.is_empty() {
        push_card(
            &mut lines,
            "Apply",
            vec![KeyValueRow::muted("Result", "nothing to remove")],
            profile.color,
        );
        push_blocked_prune_table(&mut lines, blocked, profile);
        return lines;
    }

    lines.push(String::new());
    lines.extend(render_table(
        &["Env", "Root"],
        &env_root_rows(candidates),
        profile.color,
    ));
    push_blocked_prune_table(&mut lines, blocked, profile);
    lines.push(String::new());
    lines.push(paint(
        "Re-run with --yes to remove them.",
        Tone::Muted,
        profile.color,
    ));
    lines
}

fn env_prune_preview_raw(
    older_than_days: i64,
    candidates: &[EnvSummary],
    blocked: &[BlockedPruneSummary],
) -> Vec<String> {
    let mut lines = vec![format!(
        "Prune preview ({}d): {} candidate(s)",
        older_than_days,
        candidates.len()
    )];
    for summary in candidates {
        lines.push(format!("  {}  {}", summary.name, summary.root));
    }
    push_blocked_prune_raw(&mut lines, blocked);
    lines.push("Re-run with --yes to remove them.".to_string());
    lines
}

/// `stopped` marks an apply that ended on an error after these removals.
pub fn env_pruned(
    removed: &[EnvSummary],
    blocked: &[BlockedPruneSummary],
    stopped: bool,
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return env_pruned_raw(removed, blocked, stopped);
    }

    let mut lines = vec![paint(
        if stopped {
            "Environment prune stopped"
        } else {
            "Environment prune applied"
        },
        Tone::Strong,
        profile.color,
    )];
    let mut summary = vec![KeyValueRow::plain("Removed", removed.len().to_string())];
    if !blocked.is_empty() {
        summary.push(KeyValueRow::danger("Blocked", blocked.len().to_string()));
    }
    if stopped {
        summary.push(KeyValueRow::danger(
            "Result",
            "stopped on an error; remaining candidates were not removed",
        ));
    }
    push_card(&mut lines, "Summary", summary, profile.color);
    if !removed.is_empty() {
        lines.push(String::new());
        lines.extend(render_table(
            &["Env", "Root"],
            &env_root_rows(removed),
            profile.color,
        ));
    }
    push_blocked_prune_table(&mut lines, blocked, profile);
    lines
}

fn env_pruned_raw(
    removed: &[EnvSummary],
    blocked: &[BlockedPruneSummary],
    stopped: bool,
) -> Vec<String> {
    let mut lines = vec![if stopped {
        format!(
            "Prune stopped on an error after removing {} environment(s).",
            removed.len()
        )
    } else {
        format!("Pruned {} environment(s).", removed.len())
    }];
    for summary in removed {
        lines.push(format!("  {}  {}", summary.name, summary.root));
    }
    push_blocked_prune_raw(&mut lines, blocked);
    lines
}

fn env_root_rows(envs: &[EnvSummary]) -> Vec<Vec<Cell>> {
    envs.iter()
        .map(|summary| {
            vec![
                Cell::accent(summary.name.clone()),
                Cell::muted(summary.root.clone()),
            ]
        })
        .collect()
}

fn push_blocked_prune_table(
    lines: &mut Vec<String>,
    blocked: &[BlockedPruneSummary],
    profile: RenderProfile,
) {
    if blocked.is_empty() {
        return;
    }
    let rows = blocked
        .iter()
        .map(|summary| {
            vec![
                Cell::accent(summary.env.name.clone()),
                Cell::muted(summary.env.root.clone()),
                Cell::danger(summary.reason.clone()),
            ]
        })
        .collect::<Vec<_>>();
    lines.push(String::new());
    lines.push(paint("Blocked", Tone::Strong, profile.color));
    lines.extend(render_table(
        &["Env", "Root", "Reason"],
        &rows,
        profile.color,
    ));
}

fn push_blocked_prune_raw(lines: &mut Vec<String>, blocked: &[BlockedPruneSummary]) {
    if blocked.is_empty() {
        return;
    }
    lines.push(format!("Blocked {} environment(s):", blocked.len()));
    for summary in blocked {
        lines.push(format!("  {}  {}", summary.env.name, summary.env.root));
        lines.push(format!("    {}", summary.reason));
    }
}

pub fn env_created(
    summary: &EnvSummary,
    gateway_port_source: Option<&str>,
    command_example: &str,
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return env_created_raw(summary, gateway_port_source, command_example);
    }

    let mut lines = vec![paint("Environment created", Tone::Strong, profile.color)];

    let mut environment = vec![
        KeyValueRow::accent("Name", summary.name.clone()),
        KeyValueRow::plain("Root", summary.root.clone()),
        action_gateway_port_row(summary.gateway_port, gateway_port_source),
        action_binding_row(summary),
    ];
    if summary.protected {
        environment.push(KeyValueRow::warning("Protected", "yes"));
    }
    push_card(&mut lines, "Environment", environment, profile.color);

    push_card(
        &mut lines,
        "Next",
        env_action_next_steps(summary, command_example, true),
        profile.color,
    );

    lines
}

fn env_created_raw(
    summary: &EnvSummary,
    gateway_port_source: Option<&str>,
    command_example: &str,
) -> Vec<String> {
    let mut lines = vec![
        format!("Created env {}", summary.name),
        format!("  root: {}", summary.root),
        format!("  openclaw home: {}", summary.openclaw_home),
        format!("  workspace: {}", summary.workspace_dir),
    ];
    if let Some(port) = summary.gateway_port {
        lines.push(render_gateway_port_line(port, gateway_port_source));
    }
    if let Some(runtime) = summary.default_runtime.as_deref() {
        lines.push(format!("  runtime: {runtime}"));
    }
    if let Some(launcher) = summary.default_launcher.as_deref() {
        lines.push(format!("  launcher: {launcher}"));
    }
    if summary.default_runtime.is_some() || summary.default_launcher.is_some() {
        lines.push(format!(
            "  open: {command_example} @{} -- tui",
            summary.name
        ));
        lines.push(format!(
            "  onboard: {command_example} @{} -- onboard",
            summary.name
        ));
        lines.push(format!(
            "  run: {command_example} @{} -- status",
            summary.name
        ));
    } else {
        lines.push("  next: bind a runtime or launcher before running OpenClaw".to_string());
    }
    lines
}

pub fn env_cloned(
    summary: &EnvSummary,
    gateway_port_source: Option<&str>,
    source_name: &str,
    command_example: &str,
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return env_cloned_raw(summary, gateway_port_source, source_name, command_example);
    }

    let mut lines = vec![paint("Environment cloned", Tone::Strong, profile.color)];

    push_card(
        &mut lines,
        "Environment",
        vec![
            KeyValueRow::accent("Name", summary.name.clone()),
            KeyValueRow::plain("From", source_name),
            KeyValueRow::plain("Root", summary.root.clone()),
            action_gateway_port_row(summary.gateway_port, gateway_port_source),
            action_binding_row(summary),
            KeyValueRow::muted("Service", "not copied"),
        ],
        profile.color,
    );

    push_card(
        &mut lines,
        "Next",
        env_action_next_steps(summary, command_example, true),
        profile.color,
    );

    lines
}

fn env_cloned_raw(
    summary: &EnvSummary,
    gateway_port_source: Option<&str>,
    source_name: &str,
    command_example: &str,
) -> Vec<String> {
    let mut lines = vec![
        format!("Cloned env {} from {}", summary.name, source_name),
        format!("  root: {}", summary.root),
        format!("  openclaw home: {}", summary.openclaw_home),
        format!("  workspace: {}", summary.workspace_dir),
    ];
    if let Some(port) = summary.gateway_port {
        lines.push(render_gateway_port_line(port, gateway_port_source));
    }
    lines.push("  service: not copied from source".to_string());
    lines.push(format!("  start: {command_example} start {}", summary.name));
    if summary.default_runtime.is_some() || summary.default_launcher.is_some() {
        lines.push(format!(
            "  open: {command_example} @{} -- tui",
            summary.name
        ));
        lines.push(format!(
            "  onboard: {command_example} @{} -- onboard",
            summary.name
        ));
        lines.push(format!(
            "  run: {command_example} @{} -- status",
            summary.name
        ));
    }
    lines
}

pub fn env_exported(summary: &EnvExportSummary, profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        return env_exported_raw(summary);
    }

    let mut lines = vec![paint("Environment exported", Tone::Strong, profile.color)];

    let mut export_rows = vec![
        KeyValueRow::accent("Name", summary.name.clone()),
        KeyValueRow::plain("Root", summary.root.clone()),
        KeyValueRow::plain("Archive", summary.archive_path.clone()),
        KeyValueRow::plain(
            "State included",
            if summary.include_state { "yes" } else { "no" },
        ),
        action_export_binding_row(
            summary.default_runtime.clone(),
            summary.default_launcher.clone(),
        ),
    ];
    if summary.protected {
        export_rows.push(KeyValueRow::warning("Protected", "yes"));
    }
    push_card(&mut lines, "Export", export_rows, profile.color);

    lines
}

fn env_exported_raw(summary: &EnvExportSummary) -> Vec<String> {
    let mut lines = vec![
        format!("Exported env {}", summary.name),
        format!("  root: {}", summary.root),
        format!("  archive: {}", summary.archive_path),
        format!("  state included: {}", summary.include_state),
    ];
    if let Some(runtime) = summary.default_runtime.as_deref() {
        lines.push(format!("  runtime: {runtime}"));
    }
    if let Some(launcher) = summary.default_launcher.as_deref() {
        lines.push(format!("  launcher: {launcher}"));
    }
    if summary.protected {
        lines.push("  protected: true".to_string());
    }
    lines
}

pub fn env_imported(
    summary: &EnvImportSummary,
    command_example: &str,
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return env_imported_raw(summary, command_example);
    }

    let mut lines = vec![paint("Environment imported", Tone::Strong, profile.color)];

    let mut import_rows = vec![
        KeyValueRow::accent("Name", summary.name.clone()),
        KeyValueRow::plain("From", summary.source_name.clone()),
        KeyValueRow::plain("Root", summary.root.clone()),
        KeyValueRow::plain("Archive", summary.archive_path.clone()),
        action_export_binding_row(
            summary.default_runtime.clone(),
            summary.default_launcher.clone(),
        ),
    ];
    if summary.config_paths_rewritten == Some(true) {
        import_rows.push(KeyValueRow::plain("Config paths", "rewritten for new root"));
    }
    if let Some(rewritten) = summary.state_paths_rewritten {
        import_rows.push(KeyValueRow::plain(
            "State paths",
            if rewritten {
                "rewritten for new root"
            } else {
                "unchanged"
            },
        ));
    }
    if let Some(databases) = &summary.database_paths_rewritten {
        for (database, count) in databases {
            import_rows.push(KeyValueRow::plain(
                database,
                format!("{count} text value(s) rewritten"),
            ));
        }
    }
    if summary.protected {
        import_rows.push(KeyValueRow::warning("Protected", "yes"));
    }
    push_card(&mut lines, "Environment", import_rows, profile.color);

    let mut next = vec![KeyValueRow::accent(
        "Start",
        format!("{command_example} start {}", summary.name),
    )];
    if summary.default_runtime.is_some() || summary.default_launcher.is_some() {
        next.push(KeyValueRow::accent(
            "Run",
            format!("{command_example} @{} -- status", summary.name),
        ));
        next.push(KeyValueRow::warning(
            "Onboard",
            format!("{command_example} @{} -- onboard", summary.name),
        ));
    }
    push_card(&mut lines, "Next", next, profile.color);

    lines
}

fn env_imported_raw(summary: &EnvImportSummary, command_example: &str) -> Vec<String> {
    let mut lines = vec![
        format!("Imported env {} from {}", summary.name, summary.source_name),
        format!("  root: {}", summary.root),
        format!("  archive: {}", summary.archive_path),
    ];
    if summary.config_paths_rewritten == Some(true) {
        lines.push("  config paths: rewritten for new root".to_string());
    }
    if let Some(rewritten) = summary.state_paths_rewritten {
        lines.push(format!(
            "  state paths: {}",
            if rewritten {
                "rewritten for new root"
            } else {
                "unchanged"
            }
        ));
    }
    if let Some(databases) = &summary.database_paths_rewritten {
        for (database, count) in databases {
            lines.push(format!("  {database}: {count} text value(s) rewritten"));
        }
    }
    if let Some(runtime) = summary.default_runtime.as_deref() {
        lines.push(format!("  runtime: {runtime}"));
    }
    if let Some(launcher) = summary.default_launcher.as_deref() {
        lines.push(format!("  launcher: {launcher}"));
    }
    if summary.protected {
        lines.push("  protected: true".to_string());
    }
    if summary.default_runtime.is_some() || summary.default_launcher.is_some() {
        lines.push(format!(
            "  open: {command_example} @{} -- tui",
            summary.name
        ));
        lines.push(format!(
            "  run: {command_example} @{} -- status",
            summary.name
        ));
    }
    lines
}

pub fn env_doctor(
    doctor: &EnvDoctorSummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return env_doctor_raw(doctor);
    }

    let mut lines = vec![paint(
        &format!("Environment doctor {}", doctor.env_name),
        Tone::Strong,
        profile.color,
    )];

    push_card(
        &mut lines,
        "Summary",
        vec![
            KeyValueRow::plain("Root", doctor.root.clone()),
            KeyValueRow::new(
                "Healthy",
                if doctor.healthy { "yes" } else { "no" },
                if doctor.healthy {
                    Tone::Success
                } else {
                    Tone::Danger
                },
            ),
            optional_value_row("Default runtime", doctor.default_runtime.clone()),
            optional_value_row("Default launcher", doctor.default_launcher.clone()),
            doctor_resolution_row(doctor),
        ],
        profile.color,
    );

    push_card(
        &mut lines,
        "Checks",
        vec![
            doctor_state_row("Root", &doctor.root_status),
            doctor_state_row("Config", &doctor.config_status),
            doctor_state_row("Runtime", &doctor.runtime_status),
            doctor_state_row("Launcher", &doctor.launcher_status),
            doctor_state_row("Resolution", &doctor.resolution_status),
        ],
        profile.color,
    );

    if doctor.resolved_kind.as_deref() == Some("runtime") {
        push_card(
            &mut lines,
            "Runtime",
            vec![
                optional_value_row("Source", doctor.runtime_source_kind.clone()),
                optional_value_row("Release version", doctor.runtime_release_version.clone()),
                optional_value_row("Release channel", doctor.runtime_release_channel.clone()),
            ],
            profile.color,
        );
    }

    if !doctor.issues.is_empty() {
        let issue_rows = doctor
            .issues
            .iter()
            .enumerate()
            .map(|(index, issue)| KeyValueRow::danger(format!("#{}", index + 1), issue.clone()))
            .collect::<Vec<_>>();
        push_card(&mut lines, "Issues", issue_rows, profile.color);
    }

    let next_steps = env_doctor_next_steps(doctor, command_example);
    if !next_steps.is_empty() {
        push_card(&mut lines, "Next", next_steps, profile.color);
    }

    lines
}

fn env_doctor_next_steps(doctor: &EnvDoctorSummary, command_example: &str) -> Vec<KeyValueRow> {
    if doctor.healthy {
        return vec![KeyValueRow::accent(
            "Status",
            format!("{command_example} env status {}", doctor.env_name),
        )];
    }

    if doctor.resolution_status == "unbound" {
        return vec![KeyValueRow::accent(
            "Start",
            format!("{command_example} start {}", doctor.env_name),
        )];
    }

    if doctor.resolution_status == "error" && doctor.resolved_kind.as_deref() == Some("dev") {
        return vec![KeyValueRow::warning(
            "Inspect source",
            format!("{command_example} env show {}", doctor.env_name),
        )];
    }

    if doctor.runtime_status == "missing" || doctor.runtime_status == "broken" {
        let mut rows = Vec::new();
        if let Some(runtime_name) = doctor.resolved_name.as_deref()
            && doctor.resolved_kind.as_deref() == Some("runtime")
        {
            rows.push(KeyValueRow::accent(
                "Check runtime",
                format!("{command_example} runtime verify {runtime_name}"),
            ));
        }
        rows.push(KeyValueRow::warning(
            "Cleanup",
            format!("{command_example} env cleanup {}", doctor.env_name),
        ));
        return rows;
    }

    if doctor.launcher_status == "missing" {
        return vec![
            KeyValueRow::accent("List launchers", format!("{command_example} launcher list")),
            KeyValueRow::warning(
                "Cleanup",
                format!("{command_example} env cleanup {}", doctor.env_name),
            ),
        ];
    }

    if doctor.config_status == "drifted" {
        return vec![KeyValueRow::warning(
            "Cleanup",
            format!("{command_example} env cleanup {}", doctor.env_name),
        )];
    }

    Vec::new()
}

fn env_doctor_raw(doctor: &EnvDoctorSummary) -> Vec<String> {
    let mut lines = vec![
        format!("envName: {}", doctor.env_name),
        format!("root: {}", doctor.root),
        format!("healthy: {}", doctor.healthy),
        format!("rootStatus: {}", doctor.root_status),
        format!("configStatus: {}", doctor.config_status),
        format!("runtimeStatus: {}", doctor.runtime_status),
        format!("launcherStatus: {}", doctor.launcher_status),
        format!("resolutionStatus: {}", doctor.resolution_status),
    ];
    if let Some(runtime) = doctor.default_runtime.as_deref() {
        lines.push(format!("defaultRuntime: {runtime}"));
    }
    if let Some(launcher) = doctor.default_launcher.as_deref() {
        lines.push(format!("defaultLauncher: {launcher}"));
    }
    if let Some(kind) = doctor.resolved_kind.as_deref() {
        lines.push(format!("resolvedKind: {kind}"));
    }
    if let Some(name) = doctor.resolved_name.as_deref() {
        lines.push(format!("resolvedName: {name}"));
    }
    if let Some(source_kind) = doctor.runtime_source_kind.as_deref() {
        lines.push(format!("runtimeSourceKind: {source_kind}"));
    }
    if let Some(release_version) = doctor.runtime_release_version.as_deref() {
        lines.push(format!("runtimeReleaseVersion: {release_version}"));
    }
    if let Some(release_channel) = doctor.runtime_release_channel.as_deref() {
        lines.push(format!("runtimeReleaseChannel: {release_channel}"));
    }
    for issue in &doctor.issues {
        lines.push(format!("issue: {issue}"));
    }
    lines
}

pub fn env_cleanup_batch(cleanup: &EnvCleanupBatchSummary, profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        return env_cleanup_batch_raw(cleanup);
    }

    let mut lines = vec![paint(
        if cleanup.apply {
            "Environment cleanup applied"
        } else {
            "Environment cleanup preview"
        },
        Tone::Strong,
        profile.color,
    )];
    push_card(
        &mut lines,
        "Summary",
        vec![
            KeyValueRow::plain("Scope", "all environments"),
            KeyValueRow::plain("Count", cleanup.count.to_string()),
        ],
        profile.color,
    );
    if cleanup.results.is_empty() {
        return lines;
    }

    let rows = cleanup
        .results
        .iter()
        .map(|result| {
            vec![
                Cell::accent(result.env_name.clone()),
                Cell::muted(result.root.clone()),
                Cell::plain(result.actions.len().to_string()),
            ]
        })
        .collect::<Vec<_>>();
    lines.push(String::new());
    lines.extend(render_table(
        &["Env", "Root", "Actions"],
        &rows,
        profile.color,
    ));
    for result in &cleanup.results {
        push_cleanup_removals(
            &mut lines,
            &format!(
                "{} {}",
                cleanup_removals_title(cleanup.apply),
                result.env_name
            ),
            &result.actions,
            profile.color,
        );
    }
    lines
}

fn cleanup_removals_title(apply: bool) -> &'static str {
    if apply {
        "Removed from"
    } else {
        "Removes from"
    }
}

fn push_cleanup_removals(
    lines: &mut Vec<String>,
    title: &str,
    actions: &[EnvCleanupActionSummary],
    color: bool,
) {
    let rows = actions
        .iter()
        .flat_map(|action| &action.removals)
        .map(|path| KeyValueRow::warning("Path", path.clone()))
        .collect();
    push_card(lines, title, rows, color);
}

fn push_raw_cleanup_action(
    lines: &mut Vec<String>,
    indent: &str,
    apply: bool,
    action: &EnvCleanupActionSummary,
) {
    lines.push(format!("{indent}{}: {}", action.kind, action.description));
    let verb = if apply { "removed" } else { "removes" };
    for path in &action.removals {
        lines.push(format!("{indent}  {verb}: {path}"));
    }
}

fn env_cleanup_batch_raw(cleanup: &EnvCleanupBatchSummary) -> Vec<String> {
    let mut lines = if cleanup.apply {
        vec![format!("Applied cleanup (--all): {} env(s)", cleanup.count)]
    } else {
        vec![format!("Cleanup preview (--all): {} env(s)", cleanup.count)]
    };
    for result in &cleanup.results {
        lines.push(format!("  {}", result.env_name));
        lines.push(format!("    root: {}", result.root));
        if result.apply {
            lines.push(format!("    applied fixes: {}", result.actions.len()));
        } else {
            lines.push(format!("    safe fixes: {}", result.actions.len()));
        }
        for action in &result.actions {
            push_raw_cleanup_action(&mut lines, "    ", result.apply, action);
        }
    }
    lines
}

pub fn env_cleanup(cleanup: &EnvCleanupSummary, profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        return env_cleanup_raw(cleanup);
    }

    let mut lines = vec![paint(
        if cleanup.apply {
            "Environment cleanup applied"
        } else {
            "Environment cleanup preview"
        },
        Tone::Strong,
        profile.color,
    )];
    push_card(
        &mut lines,
        "Environment",
        vec![
            KeyValueRow::accent("Env", cleanup.env_name.clone()),
            KeyValueRow::plain("Root", cleanup.root.clone()),
            KeyValueRow::plain("Actions", cleanup.actions.len().to_string()),
        ],
        profile.color,
    );
    if !cleanup.actions.is_empty() {
        let actions = cleanup
            .actions
            .iter()
            .map(|action| KeyValueRow::plain(&action.kind, action.description.clone()))
            .collect::<Vec<_>>();
        push_card(&mut lines, "Plan", actions, profile.color);
        push_cleanup_removals(
            &mut lines,
            if cleanup.apply { "Removed" } else { "Removes" },
            &cleanup.actions,
            profile.color,
        );
    }
    if cleanup.apply {
        if let Some(healthy_after) = cleanup.healthy_after {
            push_card(
                &mut lines,
                "Result",
                vec![KeyValueRow::new(
                    "Healthy",
                    if healthy_after { "yes" } else { "no" },
                    if healthy_after {
                        Tone::Success
                    } else {
                        Tone::Danger
                    },
                )],
                profile.color,
            );
        }
    } else if !cleanup.actions.is_empty() {
        push_card(
            &mut lines,
            "Apply",
            vec![KeyValueRow::warning(
                "Run",
                "re-run with --yes to apply them",
            )],
            profile.color,
        );
    }
    lines
}

fn env_cleanup_raw(cleanup: &EnvCleanupSummary) -> Vec<String> {
    let mut lines = if cleanup.apply {
        vec![format!("Applied cleanup for env {}", cleanup.env_name)]
    } else {
        vec![format!("Cleanup preview for env {}", cleanup.env_name)]
    };
    lines.push(format!("  root: {}", cleanup.root));
    if cleanup.apply {
        lines.push(format!("  applied fixes: {}", cleanup.actions.len()));
    } else {
        lines.push(format!("  safe fixes: {}", cleanup.actions.len()));
    }
    for action in &cleanup.actions {
        push_raw_cleanup_action(&mut lines, "  ", cleanup.apply, action);
    }
    if cleanup.apply {
        if let Some(healthy_after) = cleanup.healthy_after {
            lines.push(format!("  healthy after: {healthy_after}"));
        }
        if let Some(issues_after) = cleanup.issues_after.as_ref() {
            for issue in issues_after {
                lines.push(format!("  issue: {issue}"));
            }
        }
    } else {
        for issue in &cleanup.issues_before {
            lines.push(format!("  issue: {issue}"));
        }
        if !cleanup.actions.is_empty() {
            lines.push("  re-run with --yes to apply them".to_string());
        }
    }
    lines
}

pub fn env_list(summaries: &[EnvSummary], profile: RenderProfile) -> Vec<String> {
    if summaries.is_empty() {
        return vec!["No environments.".to_string()];
    }
    if !profile.pretty {
        return env_list_raw(summaries);
    }

    let rows = summaries
        .iter()
        .map(|summary| {
            let flags = if summary.protected {
                "protected"
            } else {
                "—"
            };
            vec![
                Cell::accent(summary.name.clone()),
                Cell::muted(summary.root.clone()),
                optional_cell(summary.default_runtime.as_deref(), Tone::Accent),
                optional_cell(summary.default_launcher.as_deref(), Tone::Accent),
                optional_number_cell(summary.gateway_port),
                if summary.protected {
                    Cell::warning(flags)
                } else {
                    Cell::muted(flags)
                },
            ]
        })
        .collect::<Vec<_>>();
    render_table(
        &["Name", "Root", "Runtime", "Launcher", "Port", "Flags"],
        &rows,
        profile.color,
    )
}

fn env_list_raw(summaries: &[EnvSummary]) -> Vec<String> {
    let mut lines = Vec::with_capacity(summaries.len());
    for summary in summaries {
        let mut bits = vec![summary.name.clone(), summary.root.clone()];
        if let Some(runtime) = summary.default_runtime.as_deref() {
            bits.push(format!("runtime={runtime}"));
        }
        if let Some(launcher) = summary.default_launcher.as_deref() {
            bits.push(format!("launcher={launcher}"));
        }
        if let Some(port) = summary.gateway_port {
            bits.push(format!("port={port}"));
        }
        if summary.protected {
            bits.push("protected".to_string());
        }
        lines.push(bits.join("  "));
    }
    lines
}

fn optional_cell(value: Option<&str>, tone: Tone) -> Cell {
    match value {
        Some(value) => Cell::new(value, crate::infra::terminal::Align::Left, tone),
        None => Cell::muted("—"),
    }
}

fn optional_number_cell(value: Option<u32>) -> Cell {
    match value {
        Some(value) => Cell::right(value.to_string(), Tone::Accent),
        None => Cell::muted("—"),
    }
}

fn render_gateway_port_line(port: u32, source: Option<&str>) -> String {
    match source {
        Some("metadata") | None => format!("  gateway port: {port}"),
        Some(source) => format!("  effective gateway port: {port} ({source})"),
    }
}

fn action_gateway_port_row(port: Option<u32>, source: Option<&str>) -> KeyValueRow {
    match port {
        Some(port) => match source {
            Some("metadata") | None => KeyValueRow::accent("Port", port.to_string()),
            Some(source) => KeyValueRow::accent("Port", format!("{port} ({source})")),
        },
        None => KeyValueRow::muted("Port", "—"),
    }
}

fn action_binding_row(summary: &EnvSummary) -> KeyValueRow {
    action_export_binding_row(
        summary.default_runtime.clone(),
        summary.default_launcher.clone(),
    )
}

fn action_export_binding_row(
    default_runtime: Option<String>,
    default_launcher: Option<String>,
) -> KeyValueRow {
    match (default_runtime, default_launcher) {
        (Some(runtime), _) => KeyValueRow::accent("Binding", format!("runtime:{runtime}")),
        (None, Some(launcher)) => KeyValueRow::accent("Binding", format!("launcher:{launcher}")),
        (None, None) => KeyValueRow::muted("Binding", "none yet"),
    }
}

fn env_action_next_steps(
    summary: &EnvSummary,
    command_example: &str,
    include_start: bool,
) -> Vec<KeyValueRow> {
    let mut rows = Vec::new();
    if include_start {
        rows.push(KeyValueRow::accent(
            "Start",
            format!("{command_example} start {}", summary.name),
        ));
    }
    if summary.default_runtime.is_some() || summary.default_launcher.is_some() {
        rows.push(KeyValueRow::accent(
            "Run",
            format!("{command_example} @{} -- status", summary.name),
        ));
        rows.push(KeyValueRow::warning(
            "Onboard",
            format!("{command_example} @{} -- onboard", summary.name),
        ));
    } else {
        rows.push(KeyValueRow::warning(
            "Set runtime",
            format!("{command_example} env set-runtime {} stable", summary.name),
        ));
        rows.push(KeyValueRow::warning(
            "Set launcher",
            format!("{command_example} env set-launcher {} dev", summary.name),
        ));
    }
    rows
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

fn optional_value_row(label: &str, value: Option<String>) -> KeyValueRow {
    match value {
        Some(value) => KeyValueRow::plain(label, value),
        None => KeyValueRow::muted(label, "—"),
    }
}

fn bool_row(label: &str, value: bool) -> KeyValueRow {
    if value {
        KeyValueRow::warning(label, "yes")
    } else {
        KeyValueRow::muted(label, "no")
    }
}

fn optional_state_row(label: &str, value: Option<String>) -> KeyValueRow {
    match value {
        Some(value) => KeyValueRow::new(label, value.clone(), state_tone(&value)),
        None => KeyValueRow::muted(label, "—"),
    }
}

fn resolution_row(status: &EnvStatusSummary) -> KeyValueRow {
    match (
        status.resolved_kind.as_deref(),
        status.resolved_name.as_deref(),
    ) {
        (Some(kind), Some(name)) => KeyValueRow::accent("Resolved", format!("{kind}:{name}")),
        _ => KeyValueRow::muted("Resolved", "—"),
    }
}

fn doctor_resolution_row(doctor: &EnvDoctorSummary) -> KeyValueRow {
    match (
        doctor.resolved_kind.as_deref(),
        doctor.resolved_name.as_deref(),
    ) {
        (Some(kind), Some(name)) => KeyValueRow::accent("Resolved", format!("{kind}:{name}")),
        _ => KeyValueRow::muted("Resolved", "—"),
    }
}

fn doctor_state_row(label: &str, status: &str) -> KeyValueRow {
    KeyValueRow::new(label, status, doctor_state_tone(status))
}

fn doctor_state_tone(status: &str) -> Tone {
    match status {
        "ok" => Tone::Success,
        "unbound" | "absent" => Tone::Muted,
        "missing" | "mismatch" | "invalid" | "broken" | "error" => Tone::Danger,
        _ => Tone::Warning,
    }
}

fn state_tone(state: &str) -> Tone {
    match state {
        "ok" | "running" | "match" | "healthy" => Tone::Success,
        "loaded" | "installed" | "loaded-other" | "installed-other" | "running-other" => {
            Tone::Warning
        }
        "broken" | "missing" | "unreachable" => Tone::Danger,
        "absent" | "stopped" | "unknown" => Tone::Muted,
        _ => Tone::Plain,
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use time::OffsetDateTime;

    use super::{
        RenderProfile, env_cleanup, env_cleanup_batch, env_destroy_preview, env_destroyed,
        env_doctor, env_launcher_updated, env_list, env_prune_preview, env_pruned, env_resolved,
        env_runtime_updated, env_show, env_snapshot_list_with_width, env_snapshot_prune_preview,
        env_snapshot_show, env_status,
    };
    use crate::cli::env::{BlockedPruneSummary, EnvDestroyStepSummary, EnvDestroySummary};
    use crate::env::{
        EnvCleanupActionSummary, EnvCleanupBatchSummary, EnvCleanupSummary, EnvDoctorSummary,
        EnvSnapshotSummary, EnvStatusSummary, EnvSummary, ExecutionSummary,
    };

    fn cleanup_with_reset(apply: bool) -> EnvCleanupSummary {
        EnvCleanupSummary {
            env_name: "demo".to_string(),
            root: "/tmp/demo".to_string(),
            apply,
            default_runtime: None,
            default_launcher: None,
            healthy_before: false,
            healthy_after: apply.then_some(true),
            actions: vec![EnvCleanupActionSummary {
                kind: "reset-openclaw-runtime-state".to_string(),
                description: "clear copied OpenClaw runtime state outside config and workspace"
                    .to_string(),
                applied: apply,
                removals: vec![
                    ".openclaw/credentials".to_string(),
                    ".openclaw/logs".to_string(),
                ],
            }],
            issues_before: Vec::new(),
            issues_after: apply.then(Vec::new),
        }
    }

    #[test]
    fn binding_updates_report_a_clear_differently_from_a_bind() {
        assert_eq!(
            env_runtime_updated("demo", Some("stable"), RenderProfile::raw()),
            vec!["Updated env demo: defaultRuntime=stable"]
        );
        assert_eq!(
            env_runtime_updated("demo", None, RenderProfile::raw()),
            vec!["Updated env demo: defaultRuntime cleared"]
        );
        assert_eq!(
            env_launcher_updated("demo", None, RenderProfile::raw()),
            vec!["Updated env demo: defaultLauncher cleared"]
        );
        let bound = env_launcher_updated("demo", Some("dev"), RenderProfile::pretty(false));
        assert!(
            bound
                .iter()
                .any(|line| line.contains("Launcher") && line.contains("dev"))
        );
        // A clear never matches the bind of an entry named like the clear wording.
        assert_ne!(
            env_runtime_updated("demo", None, RenderProfile::raw()),
            env_runtime_updated("demo", Some("cleared"), RenderProfile::raw())
        );
        let cleared = env_runtime_updated("demo", None, RenderProfile::pretty(false));
        assert_ne!(
            cleared,
            env_runtime_updated("demo", Some("cleared"), RenderProfile::pretty(false))
        );
        assert!(
            cleared
                .iter()
                .any(|line| line.contains("Runtime") && line.contains("(cleared)")),
            "{cleared:?}"
        );
        assert!(
            !cleared.iter().any(|line| line.contains("none")),
            "{cleared:?}"
        );
    }

    #[test]
    fn env_cleanup_lists_each_runtime_state_removal() {
        for apply in [false, true] {
            let (card, verb) = if apply {
                ("Removed", "removed")
            } else {
                ("Removes", "removes")
            };
            let cleanup = cleanup_with_reset(apply);
            let pretty = env_cleanup(&cleanup, RenderProfile::pretty(false));
            assert!(pretty.iter().any(|line| line.contains(card)), "{pretty:?}");
            for path in [".openclaw/credentials", ".openclaw/logs"] {
                assert!(pretty.iter().any(|line| line.contains(path)), "{pretty:?}");
            }
            let raw = env_cleanup(&cleanup, RenderProfile::raw());
            assert!(
                raw.contains(&format!("    {verb}: .openclaw/credentials")),
                "{raw:?}"
            );

            let batch = EnvCleanupBatchSummary {
                apply,
                count: 1,
                results: vec![cleanup],
            };
            let pretty = env_cleanup_batch(&batch, RenderProfile::pretty(false));
            assert!(
                pretty
                    .iter()
                    .any(|line| line.contains(&format!("{card} from demo"))),
                "{pretty:?}"
            );
            assert!(pretty.iter().any(|line| line.contains(".openclaw/logs")));
            let raw = env_cleanup_batch(&batch, RenderProfile::raw());
            assert!(
                raw.contains(&format!("      {verb}: .openclaw/logs")),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn env_list_pretty_uses_a_table() {
        let summaries = vec![EnvSummary {
            upgrade_independent_paths: Vec::new(),
            name: "demo".to_string(),
            root: "/tmp/demo".to_string(),
            openclaw_home: "/tmp/demo/.openclaw".to_string(),
            state_dir: "/tmp/demo/.openclaw".to_string(),
            config_path: "/tmp/demo/.openclaw/openclaw.json".to_string(),
            workspace_dir: "/tmp/demo/workspace".to_string(),
            gateway_port: Some(18789),
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: Some("stable".to_string()),
            dev_repo_root: None,
            dev_worktree_root: None,
            dev_ui_port: None,
            protected: true,
            created_at: OffsetDateTime::UNIX_EPOCH,
            last_used_at: None,
        }];

        let lines = env_list(&summaries, RenderProfile::pretty(false));
        assert!(lines[0].starts_with('┌'));
        assert!(lines[1].contains("Name"));
        assert!(lines[3].contains("demo"));
        assert!(lines[3].contains("protected"));
        assert!(lines[4].starts_with('└'));
    }

    #[test]
    fn env_show_pretty_uses_cards() {
        let lines = env_show(
            &EnvSummary {
                upgrade_independent_paths: Vec::new(),
                name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                openclaw_home: "/tmp/demo".to_string(),
                state_dir: "/tmp/demo/.openclaw".to_string(),
                config_path: "/tmp/demo/.openclaw/openclaw.json".to_string(),
                workspace_dir: "/tmp/demo/.openclaw/workspace".to_string(),
                gateway_port: Some(18789),
                service_enabled: true,
                service_running: true,
                default_runtime: None,
                default_launcher: Some("stable".to_string()),
                dev_repo_root: None,
                dev_worktree_root: None,
                dev_ui_port: None,
                protected: false,
                created_at: OffsetDateTime::UNIX_EPOCH,
                last_used_at: None,
            },
            RenderProfile::pretty(false),
            "ocm",
        )
        .unwrap();

        assert_eq!(lines[0], "Environment demo");
        assert!(lines.iter().any(|line| line.contains("Paths")));
        assert!(lines.iter().any(|line| line.contains("Metadata")));
    }

    #[test]
    fn env_show_pretty_suggests_start_for_unbound_env() {
        let lines = env_show(
            &EnvSummary {
                upgrade_independent_paths: Vec::new(),
                name: "bare".to_string(),
                root: "/tmp/bare".to_string(),
                openclaw_home: "/tmp/bare".to_string(),
                state_dir: "/tmp/bare/.openclaw".to_string(),
                config_path: "/tmp/bare/.openclaw/openclaw.json".to_string(),
                workspace_dir: "/tmp/bare/.openclaw/workspace".to_string(),
                gateway_port: Some(18789),
                service_enabled: false,
                service_running: false,
                default_runtime: None,
                default_launcher: None,
                dev_repo_root: None,
                dev_worktree_root: None,
                dev_ui_port: None,
                protected: false,
                created_at: OffsetDateTime::UNIX_EPOCH,
                last_used_at: None,
            },
            RenderProfile::pretty(false),
            "ocm",
        )
        .unwrap();

        assert!(lines.iter().any(|line| line.contains("Next")));
        assert!(lines.iter().any(|line| line.contains("ocm start bare")));
    }

    #[test]
    fn env_status_pretty_uses_cards() {
        let lines = env_status(
            &EnvStatusSummary {
                env_name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                gateway_port: Some(18789),
                gateway_port_source: Some("computed".to_string()),
                default_runtime: None,
                default_launcher: Some("stable".to_string()),
                resolved_kind: Some("launcher".to_string()),
                resolved_name: Some("stable".to_string()),
                binary_path: None,
                command: Some("openclaw".to_string()),
                run_dir: Some("/tmp/demo".to_string()),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                runtime_health: None,
                managed_service_state: Some("running".to_string()),
                openclaw_state: Some("healthy".to_string()),
                global_service_state: Some("absent".to_string()),
                service_definition_drift: None,
                service_live_exec_unverified: None,
                service_orphaned_live: None,
                service_issue: None,
                issue: None,
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert_eq!(lines[0], "Environment status demo");
        assert!(lines.iter().any(|line| line.contains("Binding")));
        assert!(lines.iter().any(|line| line.contains("Gateway")));
        assert!(lines.iter().any(|line| line.contains("OCM service")));
        assert!(lines.iter().any(|line| line.contains("OpenClaw")));
        assert!(lines.iter().any(|line| line.contains("Global service")));
    }

    #[test]
    fn env_status_pretty_suggests_start_for_unbound_env() {
        let lines = env_status(
            &EnvStatusSummary {
                env_name: "bare".to_string(),
                root: "/tmp/bare".to_string(),
                gateway_port: Some(18789),
                gateway_port_source: Some("computed".to_string()),
                default_runtime: None,
                default_launcher: None,
                resolved_kind: None,
                resolved_name: None,
                binary_path: None,
                command: None,
                run_dir: None,
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                runtime_health: None,
                managed_service_state: Some("absent".to_string()),
                openclaw_state: Some("stopped".to_string()),
                global_service_state: Some("absent".to_string()),
                service_definition_drift: None,
                service_live_exec_unverified: None,
                service_orphaned_live: None,
                service_issue: None,
                issue: Some("missing binding".to_string()),
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(lines.iter().any(|line| line.contains("Next")));
        assert!(lines.iter().any(|line| line.contains("ocm start bare")));
    }

    #[test]
    fn env_status_pretty_suggests_service_repair_for_unreachable_env() {
        let lines = env_status(
            &EnvStatusSummary {
                env_name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                gateway_port: Some(18789),
                gateway_port_source: Some("metadata".to_string()),
                default_runtime: None,
                default_launcher: Some("stable".to_string()),
                resolved_kind: Some("launcher".to_string()),
                resolved_name: Some("stable".to_string()),
                binary_path: None,
                command: Some("openclaw".to_string()),
                run_dir: Some("/tmp/demo".to_string()),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                runtime_health: None,
                managed_service_state: Some("loaded".to_string()),
                openclaw_state: Some("unreachable".to_string()),
                global_service_state: Some("absent".to_string()),
                service_definition_drift: None,
                service_live_exec_unverified: None,
                service_orphaned_live: None,
                service_issue: None,
                issue: None,
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm service restart demo"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm logs demo --tail 50"))
        );
    }

    #[test]
    fn env_status_pretty_surfaces_global_service_conflicts() {
        let lines = env_status(
            &EnvStatusSummary {
                env_name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                gateway_port: Some(18789),
                gateway_port_source: Some("metadata".to_string()),
                default_runtime: Some("stable".to_string()),
                default_launcher: None,
                resolved_kind: Some("runtime".to_string()),
                resolved_name: Some("stable".to_string()),
                binary_path: Some("/tmp/demo/openclaw".to_string()),
                command: None,
                run_dir: Some("/tmp/demo".to_string()),
                runtime_source_kind: Some("installed".to_string()),
                runtime_release_version: Some("2026.3.24".to_string()),
                runtime_release_channel: Some("stable".to_string()),
                runtime_health: Some("ok".to_string()),
                managed_service_state: Some("running".to_string()),
                openclaw_state: Some("healthy".to_string()),
                global_service_state: Some("running-other".to_string()),
                service_definition_drift: None,
                service_live_exec_unverified: None,
                service_orphaned_live: None,
                service_issue: None,
                issue: None,
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(lines.iter().any(|line| line.contains("Global service")));
        assert!(lines.iter().any(|line| line.contains("running-other")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm service discover"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm service status demo"))
        );
    }

    #[test]
    fn env_status_pretty_suggests_launcher_repair_for_a_broken_working_directory() {
        let lines = env_status(
            &EnvStatusSummary {
                env_name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                gateway_port: Some(18789),
                gateway_port_source: Some("metadata".to_string()),
                default_runtime: None,
                default_launcher: Some("dev".to_string()),
                resolved_kind: Some("launcher".to_string()),
                resolved_name: Some("dev".to_string()),
                binary_path: None,
                command: Some("pnpm openclaw".to_string()),
                run_dir: Some("/tmp/checkout".to_string()),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                runtime_health: None,
                managed_service_state: Some("absent".to_string()),
                openclaw_state: Some("stopped".to_string()),
                global_service_state: None,
                service_definition_drift: None,
                service_live_exec_unverified: None,
                service_orphaned_live: None,
                service_issue: None,
                issue: Some(
                    "launcher \"dev\" working directory does not exist: /tmp/checkout".to_string(),
                ),
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(
            lines
                .iter()
                .any(|line| line.contains("working directory does not exist: /tmp/checkout"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm env set-launcher demo <launcher>"))
        );
        assert!(!lines.iter().any(|line| line.contains("ocm @demo")));
    }

    #[test]
    fn env_status_pretty_suggests_dev_repair_for_an_unusable_checkout() {
        let lines = env_status(
            &EnvStatusSummary {
                env_name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                gateway_port: Some(18789),
                gateway_port_source: Some("metadata".to_string()),
                default_runtime: None,
                default_launcher: None,
                resolved_kind: Some("dev".to_string()),
                resolved_name: Some("dev".to_string()),
                binary_path: Some("pnpm".to_string()),
                command: Some("pnpm openclaw".to_string()),
                run_dir: Some("/tmp/checkout".to_string()),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                runtime_health: None,
                managed_service_state: Some("absent".to_string()),
                openclaw_state: Some("stopped".to_string()),
                global_service_state: None,
                service_definition_drift: None,
                service_live_exec_unverified: None,
                service_orphaned_live: None,
                service_issue: None,
                issue: Some("OpenClaw checkout not found at /tmp/checkout".to_string()),
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(
            lines
                .iter()
                .any(|line| line.contains("OpenClaw checkout not found at /tmp/checkout"))
        );
        assert!(lines.iter().any(|line| line.contains("ocm env show demo")));
        assert!(!lines.iter().any(|line| line.contains("ocm @demo")));
    }

    #[test]
    fn env_resolved_pretty_uses_cards() {
        let lines = env_resolved(
            &ExecutionSummary {
                env_name: "demo".to_string(),
                binding_kind: "launcher".to_string(),
                binding_name: "stable".to_string(),
                command: Some("openclaw".to_string()),
                binary_path: None,
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                forwarded_args: vec!["status".to_string()],
                run_dir: "/tmp/demo".to_string(),
            },
            RenderProfile::pretty(false),
        );

        assert_eq!(lines[0], "Execution plan demo");
        assert!(lines.iter().any(|line| line.contains("Resolution")));
        assert!(lines.iter().any(|line| line.contains("Forwarded args")));
    }

    #[test]
    fn env_doctor_pretty_uses_cards() {
        let lines = env_doctor(
            &EnvDoctorSummary {
                env_name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                default_runtime: None,
                default_launcher: Some("stable".to_string()),
                healthy: false,
                root_status: "ok".to_string(),
                config_status: "drifted".to_string(),
                runtime_status: "unbound".to_string(),
                launcher_status: "ok".to_string(),
                resolution_status: "ok".to_string(),
                resolved_kind: Some("launcher".to_string()),
                resolved_name: Some("stable".to_string()),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                issues: vec!["OpenClaw config drift".to_string()],
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert_eq!(lines[0], "Environment doctor demo");
        assert!(lines.iter().any(|line| line.contains("Summary")));
        assert!(lines.iter().any(|line| line.contains("Checks")));
        assert!(lines.iter().any(|line| line.contains("Issues")));
    }

    #[test]
    fn env_doctor_pretty_suggests_inspecting_an_unusable_dev_source() {
        let lines = env_doctor(
            &EnvDoctorSummary {
                env_name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                default_runtime: None,
                default_launcher: None,
                healthy: false,
                root_status: "ok".to_string(),
                config_status: "ok".to_string(),
                runtime_status: "unbound".to_string(),
                launcher_status: "unbound".to_string(),
                resolution_status: "error".to_string(),
                resolved_kind: Some("dev".to_string()),
                resolved_name: Some("dev".to_string()),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                issues: vec!["OpenClaw checkout not found at /tmp/checkout".to_string()],
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(
            lines
                .iter()
                .any(|line| line.contains("OpenClaw checkout not found at /tmp/checkout"))
        );
        assert!(lines.iter().any(|line| line.contains("ocm env show demo")));
    }

    #[test]
    fn env_doctor_pretty_shows_runtime_provenance() {
        let lines = env_doctor(
            &EnvDoctorSummary {
                env_name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                default_runtime: Some("stable".to_string()),
                default_launcher: None,
                healthy: true,
                root_status: "ok".to_string(),
                config_status: "absent".to_string(),
                runtime_status: "ok".to_string(),
                launcher_status: "unbound".to_string(),
                resolution_status: "ok".to_string(),
                resolved_kind: Some("runtime".to_string()),
                resolved_name: Some("stable".to_string()),
                runtime_source_kind: Some("installed".to_string()),
                runtime_release_version: Some("2026.3.24".to_string()),
                runtime_release_channel: Some("stable".to_string()),
                issues: Vec::new(),
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(lines.iter().any(|line| line.contains("Runtime")));
        assert!(lines.iter().any(|line| line.contains("Release version")));
        assert!(lines.iter().any(|line| line.contains("Release channel")));
    }

    #[test]
    fn env_doctor_pretty_suggests_runtime_repair_steps() {
        let lines = env_doctor(
            &EnvDoctorSummary {
                env_name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                default_runtime: Some("stable".to_string()),
                default_launcher: None,
                healthy: false,
                root_status: "ok".to_string(),
                config_status: "absent".to_string(),
                runtime_status: "broken".to_string(),
                launcher_status: "unbound".to_string(),
                resolution_status: "error".to_string(),
                resolved_kind: Some("runtime".to_string()),
                resolved_name: Some("stable".to_string()),
                runtime_source_kind: Some("installed".to_string()),
                runtime_release_version: Some("2026.3.24".to_string()),
                runtime_release_channel: Some("stable".to_string()),
                issues: vec!["runtime mismatch".to_string()],
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(lines.iter().any(|line| line.contains("Next")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm runtime verify stable"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm env cleanup demo"))
        );
    }

    #[test]
    fn env_doctor_pretty_suggests_cleanup_for_config_drift() {
        let lines = env_doctor(
            &EnvDoctorSummary {
                env_name: "demo".to_string(),
                root: "/tmp/demo".to_string(),
                default_runtime: None,
                default_launcher: Some("stable".to_string()),
                healthy: false,
                root_status: "ok".to_string(),
                config_status: "drifted".to_string(),
                runtime_status: "unbound".to_string(),
                launcher_status: "ok".to_string(),
                resolution_status: "ok".to_string(),
                resolved_kind: Some("launcher".to_string()),
                resolved_name: Some("stable".to_string()),
                runtime_source_kind: None,
                runtime_release_version: None,
                runtime_release_channel: None,
                issues: vec!["config drift".to_string()],
            },
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm env cleanup demo"))
        );
    }

    #[test]
    fn env_snapshot_show_pretty_uses_cards() {
        let lines = env_snapshot_show(
            &sample_snapshot("demo", "before-upgrade"),
            RenderProfile::pretty(false),
        )
        .unwrap();

        assert_eq!(lines[0], "Snapshot snap-001");
        assert!(lines.iter().any(|line| line.contains("Snapshot")));
        assert!(lines.iter().any(|line| line.contains("Paths")));
        assert!(lines.iter().any(|line| line.contains("Bindings")));
    }

    #[test]
    fn env_snapshot_list_pretty_uses_a_table() {
        let lines = env_snapshot_list_with_width(
            &[sample_snapshot("demo", "before-upgrade")],
            RenderProfile::pretty(false),
            Some(120),
        )
        .unwrap();

        assert!(lines[0].starts_with('┌'));
        assert!(lines[1].contains("Snapshot"));
        assert!(lines[3].contains("snap-001"));
        assert!(lines[3].contains("launcher:stable"));
        assert!(lines[4].starts_with('└'));
    }

    #[test]
    fn env_snapshot_prune_preview_pretty_uses_a_table() {
        let lines = env_snapshot_prune_preview(
            "all",
            &[sample_snapshot("demo", "before-upgrade")],
            RenderProfile::pretty(false),
        )
        .unwrap();

        assert_eq!(lines[0], "Snapshot prune preview");
        assert!(lines[1].contains("[scope:all]"));
        assert!(lines[1].contains("[1 candidate(s)]"));
        assert!(lines.iter().any(|line| line.starts_with('┌')));
        assert!(lines.iter().any(|line| line.contains("Checkpoint")));
        assert_eq!(lines.last().unwrap(), "Re-run with --yes to remove them.");
    }

    #[test]
    fn env_destroy_preview_pretty_uses_cards() {
        let lines = env_destroy_preview(
            &sample_destroy_summary(false),
            RenderProfile::pretty(false),
            "ocm",
        );

        assert_eq!(lines[0], "Destroy env demo");
        assert!(lines.iter().any(|line| line.contains("Summary")));
        assert!(lines.iter().any(|line| line.contains("Plan")));
        assert!(lines.iter().any(|line| line.contains("Apply")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm env destroy demo --yes"))
        );
    }

    #[test]
    fn env_destroy_preview_pretty_shows_blockers() {
        let lines = env_destroy_preview(
            &sample_destroy_summary(true),
            RenderProfile::pretty(false),
            "ocm",
        );

        assert!(lines.iter().any(|line| line.contains("Blocked")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("manual review required before destroying this env"))
        );
    }

    #[test]
    fn env_prune_pretty_shows_blocked_candidates_and_stopped_apply() {
        let summary = |name: &str| EnvSummary {
            upgrade_independent_paths: Vec::new(),
            name: name.to_string(),
            root: format!("/tmp/{name}"),
            openclaw_home: format!("/tmp/{name}/.openclaw"),
            state_dir: format!("/tmp/{name}/.openclaw"),
            config_path: format!("/tmp/{name}/.openclaw/openclaw.json"),
            workspace_dir: format!("/tmp/{name}/workspace"),
            gateway_port: None,
            service_enabled: false,
            service_running: false,
            default_runtime: None,
            default_launcher: None,
            dev_repo_root: None,
            dev_worktree_root: None,
            dev_ui_port: None,
            protected: false,
            created_at: OffsetDateTime::UNIX_EPOCH,
            last_used_at: None,
        };
        let blocked = [BlockedPruneSummary {
            env: summary("donor"),
            reason: "would affect registered dev source".to_string(),
        }];
        let has = |lines: &[String], text: &str| lines.iter().any(|line| line.contains(text));

        let preview = env_prune_preview(14, &[], &blocked, RenderProfile::pretty(false));
        assert!(has(&preview, "nothing to remove"));
        assert!(has(&preview, "Reason"));
        assert!(has(&preview, "would affect registered dev source"));

        let stopped = env_pruned(
            &[summary("plain")],
            &blocked,
            true,
            RenderProfile::pretty(false),
        );
        assert_eq!(stopped[0], "Environment prune stopped");
        assert!(has(&stopped, "remaining candidates were not removed"));
        assert!(has(&stopped, "plain") && has(&stopped, "donor"));
    }

    #[test]
    fn env_destroyed_pretty_uses_cards() {
        let mut summary = sample_destroy_summary(false);
        summary.snapshots_removed = 2;
        summary.service_uninstalled = true;
        summary.removed = true;

        let lines = env_destroyed(&summary, RenderProfile::pretty(false), "ocm");
        assert_eq!(lines[0], "Destroyed env demo");
        assert!(lines.iter().any(|line| line.contains("Removed")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Environment service") && line.contains("uninstalled"))
        );
        assert!(!lines.iter().any(|line| line.contains("OCM service")));
        assert!(lines.iter().any(|line| line.contains("Next")));
        assert!(lines.iter().any(|line| line.contains("ocm env list")));
        assert!(lines.iter().any(|line| line.contains("ocm start demo")));
    }

    fn sample_snapshot(env_name: &str, label: &str) -> EnvSnapshotSummary {
        EnvSnapshotSummary {
            upgrade_scope: None,
            id: "snap-001".to_string(),
            env_name: env_name.to_string(),
            label: Some(label.to_string()),
            archive_path: "/tmp/demo-snapshot.tar".to_string(),
            storage_kind: "tar-archive-v1".to_string(),
            source_root: "/tmp/demo".to_string(),
            gateway_port: Some(18789),
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: Some("stable".to_string()),
            protected: true,
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn sample_destroy_summary(blocked: bool) -> EnvDestroySummary {
        EnvDestroySummary {
            env_name: "demo".to_string(),
            root: "/tmp/demo".to_string(),
            dev_worktree: Some("/tmp/openclaw/.worktrees/demo".to_string()),
            protected: false,
            apply: false,
            force: false,
            snapshot_count: 2,
            service_installed: true,
            service_loaded: true,
            service_running: false,
            service_label: "ocm".to_string(),
            source_watch_pid: None,
            source_watch_stopped: false,
            source_watch_session: None,
            process_count: 0,
            process_inspection_deferred: false,
            process_candidates: Vec::new(),
            state_token: "v1:test".to_string(),
            code: None,
            blockers: if blocked {
                vec!["manual review required before destroying this env".to_string()]
            } else {
                Vec::new()
            },
            steps: vec![
                EnvDestroyStepSummary {
                    kind: "snapshots".to_string(),
                    description: "remove 2 env snapshot(s)".to_string(),
                },
                EnvDestroyStepSummary {
                    kind: "service".to_string(),
                    description: "remove OCM service ai.openclaw.ocm".to_string(),
                },
                EnvDestroyStepSummary {
                    kind: "env".to_string(),
                    description: "remove env root and metadata".to_string(),
                },
            ],
            snapshots_removed: 0,
            warnings: Vec::new(),
            service_uninstalled: false,
            processes_terminated: 0,
            worktree_removed: false,
            removed: false,
        }
    }
}

pub fn env_show(
    summary: &EnvSummary,
    profile: RenderProfile,
    command_example: &str,
) -> Result<Vec<String>, String> {
    if !profile.pretty {
        return env_show_raw(summary);
    }

    let mut lines = vec![paint(
        &format!("Environment {}", summary.name),
        Tone::Strong,
        profile.color,
    )];

    push_card(
        &mut lines,
        "Paths",
        vec![
            KeyValueRow::plain("Root", summary.root.clone()),
            KeyValueRow::plain("OpenClaw home", summary.openclaw_home.clone()),
            KeyValueRow::plain("State dir", summary.state_dir.clone()),
            KeyValueRow::plain("Config path", summary.config_path.clone()),
            KeyValueRow::plain("Workspace", summary.workspace_dir.clone()),
        ],
        profile.color,
    );

    let mut metadata = vec![
        optional_value_row(
            "Gateway port",
            summary.gateway_port.map(|value| value.to_string()),
        ),
        optional_value_row("Runtime", summary.default_runtime.clone()),
        optional_value_row("Launcher", summary.default_launcher.clone()),
        bool_row("Protected", summary.protected),
        KeyValueRow::plain("Created", format_rfc3339(summary.created_at)?),
    ];
    if let Some(last_used_at) = summary.last_used_at {
        metadata.push(KeyValueRow::plain(
            "Last used",
            format_rfc3339(last_used_at)?,
        ));
    }
    push_card(&mut lines, "Metadata", metadata, profile.color);

    let next_steps = env_show_next_steps(summary, command_example);
    if !next_steps.is_empty() {
        push_card(&mut lines, "Next", next_steps, profile.color);
    }

    Ok(lines)
}

fn env_show_next_steps(summary: &EnvSummary, command_example: &str) -> Vec<KeyValueRow> {
    if summary.default_runtime.is_none() && summary.default_launcher.is_none() {
        return vec![KeyValueRow::accent(
            "Start",
            format!("{command_example} start {}", summary.name),
        )];
    }

    vec![
        KeyValueRow::accent(
            "Open",
            format!("{command_example} @{} -- tui", summary.name),
        ),
        KeyValueRow::accent(
            "Status",
            format!("{command_example} @{} -- status", summary.name),
        ),
        KeyValueRow::warning(
            "Onboard",
            format!("{command_example} @{} -- onboard", summary.name),
        ),
    ]
}

fn env_show_raw(summary: &EnvSummary) -> Result<Vec<String>, String> {
    let mut lines = BTreeMap::new();
    lines.insert("name".to_string(), summary.name.clone());
    lines.insert("root".to_string(), summary.root.clone());
    lines.insert("openclawHome".to_string(), summary.openclaw_home.clone());
    lines.insert("stateDir".to_string(), summary.state_dir.clone());
    lines.insert("configPath".to_string(), summary.config_path.clone());
    lines.insert("workspaceDir".to_string(), summary.workspace_dir.clone());
    lines.insert("protected".to_string(), summary.protected.to_string());
    lines.insert("createdAt".to_string(), format_rfc3339(summary.created_at)?);
    if let Some(port) = summary.gateway_port {
        lines.insert("gatewayPort".to_string(), port.to_string());
    }
    if let Some(runtime) = summary.default_runtime.as_deref() {
        lines.insert("defaultRuntime".to_string(), runtime.to_string());
    }
    if let Some(launcher) = summary.default_launcher.as_deref() {
        lines.insert("defaultLauncher".to_string(), launcher.to_string());
    }
    if let Some(last_used_at) = summary.last_used_at {
        lines.insert("lastUsedAt".to_string(), format_rfc3339(last_used_at)?);
    }
    Ok(format_key_value_lines(lines))
}

pub fn env_status(
    status: &EnvStatusSummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return env_status_raw(status);
    }

    let mut lines = vec![paint(
        &format!("Environment status {}", status.env_name),
        Tone::Strong,
        profile.color,
    )];

    push_card(
        &mut lines,
        "Binding",
        vec![
            optional_value_row("Default runtime", status.default_runtime.clone()),
            optional_value_row("Default launcher", status.default_launcher.clone()),
            resolution_row(status),
            optional_value_row("Run dir", status.run_dir.clone()),
        ],
        profile.color,
    );

    let mut execution = Vec::new();
    if let Some(command) = status.command.as_ref() {
        execution.push(KeyValueRow::accent("Command", command.clone()));
    }
    if let Some(binary_path) = status.binary_path.as_ref() {
        execution.push(KeyValueRow::accent("Binary", binary_path.clone()));
    }
    if let Some(source_kind) = status.runtime_source_kind.as_ref() {
        execution.push(KeyValueRow::plain("Runtime source", source_kind.clone()));
    }
    if let Some(release_version) = status.runtime_release_version.as_ref() {
        execution.push(KeyValueRow::plain(
            "Release version",
            release_version.clone(),
        ));
    }
    if let Some(release_channel) = status.runtime_release_channel.as_ref() {
        execution.push(KeyValueRow::plain(
            "Release channel",
            release_channel.clone(),
        ));
    }
    if let Some(runtime_health) = status.runtime_health.as_ref() {
        execution.push(KeyValueRow::new(
            "Runtime health",
            runtime_health.clone(),
            state_tone(runtime_health),
        ));
    }
    if !execution.is_empty() {
        push_card(&mut lines, "Execution", execution, profile.color);
    }

    push_card(
        &mut lines,
        "Gateway",
        vec![
            optional_value_row("Port", status.gateway_port.map(|value| value.to_string())),
            optional_value_row("Port source", status.gateway_port_source.clone()),
            optional_state_row("OCM service", status.managed_service_state.clone()),
            optional_state_row("OpenClaw", status.openclaw_state.clone()),
            optional_state_row("Global service", status.global_service_state.clone()),
            optional_state_row(
                "Service drift",
                status
                    .service_definition_drift
                    .and_then(|value| value.then_some("definition-drift".to_string())),
            ),
            optional_state_row(
                "Service live state",
                status
                    .service_live_exec_unverified
                    .and_then(|value| value.then_some("unverified".to_string()))
                    .or_else(|| {
                        status
                            .service_orphaned_live
                            .and_then(|value| value.then_some("orphaned-live".to_string()))
                    }),
            ),
            KeyValueRow::plain("Root", status.root.clone()),
        ],
        profile.color,
    );

    let mut issues = Vec::new();
    if let Some(issue) = status.issue.as_ref() {
        issues.push(KeyValueRow::danger("Problem", issue.clone()));
    }
    if let Some(service_issue) = status.service_issue.as_ref() {
        issues.push(KeyValueRow::warning("Service", service_issue.clone()));
    }
    if !issues.is_empty() {
        push_card(&mut lines, "Issue", issues, profile.color);
    }

    let next_steps = env_status_next_steps(status, command_example);
    if !next_steps.is_empty() {
        push_card(&mut lines, "Next", next_steps, profile.color);
    }

    lines
}

fn env_status_next_steps(status: &EnvStatusSummary, command_example: &str) -> Vec<KeyValueRow> {
    if status.openclaw_state.as_deref() == Some("unknown") {
        return vec![KeyValueRow::warning(
            "Inspect ownership",
            format!("{command_example} service recover {}", status.env_name),
        )];
    }

    // OCM refuses service install, start and restart while the watch is active.
    if status.resolved_kind.as_deref() == Some("source-watch") {
        return vec![KeyValueRow::accent(
            "Dev session",
            format!("{command_example} dev status {}", status.env_name),
        )];
    }

    if status.resolved_kind.is_none() {
        return vec![KeyValueRow::accent(
            "Start",
            format!("{command_example} start {}", status.env_name),
        )];
    }

    if matches!(
        status.global_service_state.as_deref(),
        Some("installed-other" | "loaded-other" | "running-other")
    ) {
        return vec![
            KeyValueRow::warning(
                "Inspect global",
                format!("{command_example} service discover"),
            ),
            KeyValueRow::accent(
                "Inspect env",
                format!("{command_example} service status {}", status.env_name),
            ),
        ];
    }

    if status.service_definition_drift == Some(true) || status.service_orphaned_live == Some(true) {
        return vec![
            KeyValueRow::warning(
                "Refresh service",
                format!("{command_example} service restart {}", status.env_name),
            ),
            KeyValueRow::accent(
                "Inspect service",
                format!("{command_example} service status {}", status.env_name),
            ),
        ];
    }

    if status.service_live_exec_unverified == Some(true) {
        return vec![KeyValueRow::accent(
            "Inspect service",
            format!("{command_example} service status {}", status.env_name),
        )];
    }

    if status.resolved_kind.as_deref() == Some("runtime")
        && matches!(status.runtime_health.as_deref(), Some("missing" | "broken"))
    {
        let mut rows = Vec::new();
        if let Some(runtime_name) = status.resolved_name.as_deref() {
            rows.push(KeyValueRow::accent(
                "Check runtime",
                format!("{command_example} runtime verify {runtime_name}"),
            ));
        }
        if let Some(channel) = status.runtime_release_channel.as_deref() {
            rows.push(KeyValueRow::warning(
                "Repair binding",
                format!(
                    "{command_example} env set-runtime {} --channel {channel}",
                    status.env_name
                ),
            ));
        } else if let Some(version) = status.runtime_release_version.as_deref() {
            rows.push(KeyValueRow::warning(
                "Repair binding",
                format!(
                    "{command_example} env set-runtime {} --version {version}",
                    status.env_name
                ),
            ));
        }
        if !rows.is_empty() {
            return rows;
        }
    }

    if status.resolved_kind.as_deref() == Some("launcher") && status.issue.is_some() {
        return vec![
            KeyValueRow::accent("List launchers", format!("{command_example} launcher list")),
            KeyValueRow::warning(
                "Rebind",
                format!(
                    "{command_example} env set-launcher {} <launcher>",
                    status.env_name
                ),
            ),
        ];
    }

    // Older owned worktree bindings cannot use dev rebind, so only point at the
    // recorded source; the issue says to restore it.
    if status.resolved_kind.as_deref() == Some("dev") && status.issue.is_some() {
        return vec![KeyValueRow::warning(
            "Inspect source",
            format!("{command_example} env show {}", status.env_name),
        )];
    }

    match status.managed_service_state.as_deref() {
        Some("absent") | None => {
            if status.openclaw_state.as_deref() == Some("healthy") {
                return vec![KeyValueRow::warning(
                    "Keep running",
                    format!("{command_example} service install {}", status.env_name),
                )];
            }

            if status.resolved_kind.is_some() {
                return vec![
                    KeyValueRow::accent(
                        "Run now",
                        format!("{command_example} @{} -- status", status.env_name),
                    ),
                    KeyValueRow::warning(
                        "Keep running",
                        format!("{command_example} service install {}", status.env_name),
                    ),
                ];
            }
        }
        Some("installed") => {
            return vec![KeyValueRow::accent(
                "Start service",
                format!("{command_example} service start {}", status.env_name),
            )];
        }
        Some("loaded") | Some("running") => match status.openclaw_state.as_deref() {
            Some("auth-required") => {
                return vec![
                    KeyValueRow::warning(
                        "Repair",
                        format!("{command_example} @{} -- onboard", status.env_name),
                    ),
                    KeyValueRow::accent(
                        "Logs",
                        format!("{command_example} logs {} --tail 50", status.env_name),
                    ),
                ];
            }
            Some("stopped" | "unreachable" | "responding-but-invalid" | "wrong-service") => {
                return vec![
                    KeyValueRow::warning(
                        "Restart",
                        format!("{command_example} service restart {}", status.env_name),
                    ),
                    KeyValueRow::accent(
                        "Logs",
                        format!("{command_example} logs {} --tail 50", status.env_name),
                    ),
                ];
            }
            _ => {}
        },
        _ => {}
    }

    Vec::new()
}

fn env_status_raw(status: &EnvStatusSummary) -> Vec<String> {
    let mut lines = vec![
        format!("envName: {}", status.env_name),
        format!("root: {}", status.root),
    ];
    if let Some(port) = status.gateway_port {
        lines.push(format!("gatewayPort: {port}"));
    }
    if let Some(source) = status.gateway_port_source.as_deref() {
        lines.push(format!("gatewayPortSource: {source}"));
    }
    if let Some(runtime) = status.default_runtime.as_deref() {
        lines.push(format!("defaultRuntime: {runtime}"));
    }
    if let Some(launcher) = status.default_launcher.as_deref() {
        lines.push(format!("defaultLauncher: {launcher}"));
    }
    if let Some(kind) = status.resolved_kind.as_deref() {
        lines.push(format!("resolvedKind: {kind}"));
    }
    if let Some(name) = status.resolved_name.as_deref() {
        lines.push(format!("resolvedName: {name}"));
    }
    if let Some(binary_path) = status.binary_path.as_deref() {
        lines.push(format!("binaryPath: {binary_path}"));
    }
    if let Some(command) = status.command.as_deref() {
        lines.push(format!("command: {command}"));
    }
    if let Some(run_dir) = status.run_dir.as_deref() {
        lines.push(format!("runDir: {run_dir}"));
    }
    if let Some(source_kind) = status.runtime_source_kind.as_deref() {
        lines.push(format!("runtimeSourceKind: {source_kind}"));
    }
    if let Some(release_version) = status.runtime_release_version.as_deref() {
        lines.push(format!("runtimeReleaseVersion: {release_version}"));
    }
    if let Some(release_channel) = status.runtime_release_channel.as_deref() {
        lines.push(format!("runtimeReleaseChannel: {release_channel}"));
    }
    if let Some(runtime_health) = status.runtime_health.as_deref() {
        lines.push(format!("runtimeHealth: {runtime_health}"));
    }
    if let Some(state) = status.managed_service_state.as_deref() {
        lines.push(format!("managedServiceState: {state}"));
    }
    if let Some(state) = status.openclaw_state.as_deref() {
        lines.push(format!("openclawState: {state}"));
    }
    if let Some(state) = status.global_service_state.as_deref() {
        lines.push(format!("globalServiceState: {state}"));
    }
    if let Some(value) = status.service_definition_drift {
        lines.push(format!("serviceDefinitionDrift: {value}"));
    }
    if let Some(value) = status.service_live_exec_unverified {
        lines.push(format!("serviceLiveExecUnverified: {value}"));
    }
    if let Some(value) = status.service_orphaned_live {
        lines.push(format!("serviceOrphanedLive: {value}"));
    }
    if let Some(service_issue) = status.service_issue.as_deref() {
        lines.push(format!("serviceIssue: {service_issue}"));
    }
    if let Some(issue) = status.issue.as_deref() {
        lines.push(format!("issue: {issue}"));
    }
    lines
}

pub fn env_snapshot_created(snapshot: &EnvSnapshotSummary, profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        return env_snapshot_created_raw(snapshot);
    }

    let mut lines = vec![paint("Snapshot created", Tone::Strong, profile.color)];
    let mut rows = vec![
        KeyValueRow::accent("Env", snapshot.env_name.clone()),
        KeyValueRow::accent("Snapshot", snapshot.id.clone()),
        KeyValueRow::plain("Checkpoint", snapshot.archive_path.clone()),
        KeyValueRow::plain("Storage", snapshot.storage_kind.clone()),
    ];
    if let Some(label) = snapshot.label.as_deref() {
        rows.push(KeyValueRow::plain("Label", label));
    }
    push_card(&mut lines, "Snapshot", rows, profile.color);
    lines
}

fn env_snapshot_created_raw(snapshot: &EnvSnapshotSummary) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Created snapshot {} for env {}",
            snapshot.id, snapshot.env_name
        ),
        format!("  checkpoint: {}", snapshot.archive_path),
        format!("  storage: {}", snapshot.storage_kind),
        format!("  root: {}", snapshot.source_root),
    ];
    if let Some(label) = snapshot.label.as_deref() {
        lines.push(format!("  label: {label}"));
    }
    lines
}

pub fn env_snapshot_show(
    snapshot: &EnvSnapshotSummary,
    profile: RenderProfile,
) -> Result<Vec<String>, String> {
    if !profile.pretty {
        return env_snapshot_show_raw(snapshot);
    }

    let mut lines = vec![paint(
        &format!("Snapshot {}", snapshot.id),
        Tone::Strong,
        profile.color,
    )];

    push_card(
        &mut lines,
        "Snapshot",
        vec![
            KeyValueRow::accent("Env", snapshot.env_name.clone()),
            KeyValueRow::plain("Created", format_rfc3339(snapshot.created_at)?),
            optional_value_row("Label", snapshot.label.clone()),
            bool_row("Protected", snapshot.protected),
        ],
        profile.color,
    );

    push_card(
        &mut lines,
        "Paths",
        vec![
            KeyValueRow::plain("Checkpoint", snapshot.archive_path.clone()),
            KeyValueRow::plain("Storage", snapshot.storage_kind.clone()),
            KeyValueRow::plain("Source root", snapshot.source_root.clone()),
        ],
        profile.color,
    );

    push_card(
        &mut lines,
        "Bindings",
        vec![
            optional_value_row(
                "Gateway port",
                snapshot.gateway_port.map(|value| value.to_string()),
            ),
            optional_value_row("Runtime", snapshot.default_runtime.clone()),
            optional_value_row("Launcher", snapshot.default_launcher.clone()),
        ],
        profile.color,
    );

    if let Some(scope) = &snapshot.upgrade_scope {
        push_card(
            &mut lines,
            "Upgrade checkpoint",
            vec![KeyValueRow::plain(
                "Preserved on restore",
                independent_path_names(&scope.independent_paths),
            )],
            profile.color,
        );
    }

    Ok(lines)
}

fn independent_path_names(paths: &[std::path::PathBuf]) -> String {
    if paths.is_empty() {
        "none (whole environment)".to_string()
    } else {
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn env_snapshot_show_raw(snapshot: &EnvSnapshotSummary) -> Result<Vec<String>, String> {
    let mut lines = vec![
        format!("snapshotId: {}", snapshot.id),
        format!("envName: {}", snapshot.env_name),
        format!("archivePath: {}", snapshot.archive_path),
        format!("storageKind: {}", snapshot.storage_kind),
        format!("sourceRoot: {}", snapshot.source_root),
    ];
    if let Some(scope) = &snapshot.upgrade_scope {
        lines.push("purpose: upgrade checkpoint".to_string());
        lines.push(format!(
            "preservedOnRestore: {}",
            independent_path_names(&scope.independent_paths)
        ));
    }
    if let Some(label) = snapshot.label.as_deref() {
        lines.push(format!("label: {label}"));
    }
    if let Some(port) = snapshot.gateway_port {
        lines.push(format!("gatewayPort: {port}"));
    }
    if let Some(runtime) = snapshot.default_runtime.as_deref() {
        lines.push(format!("defaultRuntime: {runtime}"));
    }
    if let Some(launcher) = snapshot.default_launcher.as_deref() {
        lines.push(format!("defaultLauncher: {launcher}"));
    }
    if snapshot.protected {
        lines.push("protected: true".to_string());
    }
    lines.push(format!(
        "createdAt: {}",
        format_rfc3339(snapshot.created_at)?
    ));
    Ok(lines)
}

pub fn env_snapshot_list(
    snapshots: &[EnvSnapshotSummary],
    profile: RenderProfile,
) -> Result<Vec<String>, String> {
    env_snapshot_list_with_width(snapshots, profile, terminal_width())
}

fn env_snapshot_list_with_width(
    snapshots: &[EnvSnapshotSummary],
    profile: RenderProfile,
    width: Option<usize>,
) -> Result<Vec<String>, String> {
    if snapshots.is_empty() {
        return Ok(vec!["No snapshots.".to_string()]);
    }
    if !profile.pretty {
        return Ok(env_snapshot_list_raw(snapshots));
    }

    let rows = snapshots
        .iter()
        .map(|snapshot| {
            Ok(vec![
                Cell::accent(snapshot.id.clone()),
                Cell::plain(snapshot.env_name.clone()),
                optional_cell(snapshot.label.as_deref(), Tone::Accent),
                optional_number_cell(snapshot.gateway_port),
                snapshot_binding_cell(snapshot),
                Cell::muted(format_rfc3339(snapshot.created_at)?),
            ])
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(render_table_with_limit(
        &["Snapshot", "Env", "Label", "Port", "Binding", "Created"],
        &rows,
        profile.color,
        width,
    ))
}

fn env_snapshot_list_raw(snapshots: &[EnvSnapshotSummary]) -> Vec<String> {
    let mut lines = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        let mut bits = vec![snapshot.id.clone(), snapshot.env_name.clone()];
        if let Some(label) = snapshot.label.as_deref() {
            bits.push(format!("label={label}"));
        }
        bits.push(snapshot.archive_path.clone());
        lines.push(bits.join("  "));
    }
    lines
}

pub fn env_snapshot_restored(
    restored: &EnvSnapshotRestoreSummary,
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return env_snapshot_restored_raw(restored);
    }

    let mut lines = vec![paint("Snapshot restored", Tone::Strong, profile.color)];
    let mut rows = vec![
        KeyValueRow::accent("Env", restored.env_name.clone()),
        KeyValueRow::accent("Snapshot", restored.snapshot_id.clone()),
        KeyValueRow::plain("Root", restored.root.clone()),
        KeyValueRow::plain("Checkpoint", restored.archive_path.clone()),
        KeyValueRow::plain("Storage", restored.storage_kind.clone()),
        action_export_binding_row(
            restored.default_runtime.clone(),
            restored.default_launcher.clone(),
        ),
    ];
    if let Some(label) = restored.label.as_deref() {
        rows.push(KeyValueRow::plain("Label", label));
    }
    if restored.protected {
        rows.push(KeyValueRow::warning("Protected", "yes"));
    }
    push_card(&mut lines, "Environment", rows, profile.color);
    if !restored.warnings.is_empty() {
        let rows = restored
            .warnings
            .iter()
            .map(|warning| KeyValueRow::warning("Warning", warning.clone()))
            .collect::<Vec<_>>();
        lines.extend(render_key_value_card("Warnings", &rows, profile.color));
    }
    lines
}

fn env_snapshot_restored_raw(restored: &EnvSnapshotRestoreSummary) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Restored env {} from snapshot {}",
            restored.env_name, restored.snapshot_id
        ),
        format!("  root: {}", restored.root),
        format!("  checkpoint: {}", restored.archive_path),
        format!("  storage: {}", restored.storage_kind),
    ];
    if let Some(label) = restored.label.as_deref() {
        lines.push(format!("  label: {label}"));
    }
    if let Some(runtime) = restored.default_runtime.as_deref() {
        lines.push(format!("  runtime: {runtime}"));
    }
    if let Some(launcher) = restored.default_launcher.as_deref() {
        lines.push(format!("  launcher: {launcher}"));
    }
    if restored.protected {
        lines.push("  protected: true".to_string());
    }
    lines.extend(
        restored
            .warnings
            .iter()
            .map(|warning| format!("  warning: {warning}")),
    );
    lines
}

pub fn env_snapshot_removed(
    removed: &EnvSnapshotRemoveSummary,
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return env_snapshot_removed_raw(removed);
    }

    let mut lines = vec![paint("Snapshot removed", Tone::Strong, profile.color)];
    let mut rows = vec![
        KeyValueRow::accent("Env", removed.env_name.clone()),
        KeyValueRow::accent("Snapshot", removed.snapshot_id.clone()),
        KeyValueRow::plain("Checkpoint", removed.archive_path.clone()),
        KeyValueRow::plain("Storage", removed.storage_kind.clone()),
    ];
    if let Some(label) = removed.label.as_deref() {
        rows.push(KeyValueRow::plain("Label", label));
    }
    push_card(&mut lines, "Snapshot", rows, profile.color);
    if !removed.warnings.is_empty() {
        let rows = removed
            .warnings
            .iter()
            .map(|warning| KeyValueRow::warning("Warning", warning.clone()))
            .collect::<Vec<_>>();
        lines.extend(render_key_value_card("Warnings", &rows, profile.color));
    }
    lines
}

fn env_snapshot_removed_raw(removed: &EnvSnapshotRemoveSummary) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Removed snapshot {} for env {}",
            removed.snapshot_id, removed.env_name
        ),
        format!("  checkpoint: {}", removed.archive_path),
        format!("  storage: {}", removed.storage_kind),
    ];
    if let Some(label) = removed.label.as_deref() {
        lines.push(format!("  label: {label}"));
    }
    lines.extend(
        removed
            .warnings
            .iter()
            .map(|warning| format!("warning: {warning}")),
    );
    lines
}

pub fn env_snapshot_prune_preview(
    scope_label: &str,
    candidates: &[EnvSnapshotSummary],
    profile: RenderProfile,
) -> Result<Vec<String>, String> {
    if !profile.pretty {
        return Ok(env_snapshot_prune_preview_raw(scope_label, candidates));
    }

    let mut lines = vec![paint("Snapshot prune preview", Tone::Strong, profile.color)];
    lines.push(render_tags(
        &[
            Cell::accent(format!("scope:{scope_label}")),
            Cell::warning(format!("{} candidate(s)", candidates.len())),
        ],
        profile.color,
    ));

    if candidates.is_empty() {
        lines.push(String::new());
        lines.push(paint(
            "Nothing would be removed.",
            Tone::Muted,
            profile.color,
        ));
        return Ok(lines);
    }

    lines.push(String::new());
    let rows = candidates
        .iter()
        .map(|candidate| {
            Ok(vec![
                Cell::accent(candidate.id.clone()),
                Cell::plain(candidate.env_name.clone()),
                optional_cell(candidate.label.as_deref(), Tone::Accent),
                Cell::muted(format_rfc3339(candidate.created_at)?),
                Cell::muted(candidate.archive_path.clone()),
            ])
        })
        .collect::<Result<Vec<_>, String>>()?;
    lines.extend(render_table(
        &["Snapshot", "Env", "Label", "Created", "Checkpoint"],
        &rows,
        profile.color,
    ));
    lines.push(String::new());
    lines.push(paint(
        "Re-run with --yes to remove them.",
        Tone::Muted,
        profile.color,
    ));
    Ok(lines)
}

fn env_snapshot_prune_preview_raw(
    scope_label: &str,
    candidates: &[EnvSnapshotSummary],
) -> Vec<String> {
    let mut lines = vec![format!(
        "Snapshot prune preview ({scope_label}): {} candidate(s)",
        candidates.len()
    )];
    for candidate in candidates {
        let mut bits = vec![candidate.id.clone(), candidate.env_name.clone()];
        if let Some(label) = candidate.label.as_deref() {
            bits.push(format!("label={label}"));
        }
        bits.push(candidate.archive_path.clone());
        lines.push(bits.join("  "));
    }
    lines.push("Re-run with --yes to remove them.".to_string());
    lines
}

pub fn env_snapshot_pruned(
    removed: &[EnvSnapshotRemoveSummary],
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return env_snapshot_pruned_raw(removed);
    }

    let mut lines = vec![paint("Snapshot prune applied", Tone::Strong, profile.color)];
    lines.push(render_tags(
        &[Cell::warning(format!("{} removed", removed.len()))],
        profile.color,
    ));

    if removed.is_empty() {
        lines.push(String::new());
        lines.push(paint("Nothing was removed.", Tone::Muted, profile.color));
        return lines;
    }

    lines.push(String::new());
    let rows = removed
        .iter()
        .map(|snapshot| {
            vec![
                Cell::accent(snapshot.snapshot_id.clone()),
                Cell::plain(snapshot.env_name.clone()),
                optional_cell(snapshot.label.as_deref(), Tone::Accent),
                Cell::muted(snapshot.archive_path.clone()),
            ]
        })
        .collect::<Vec<_>>();
    lines.extend(render_table(
        &["Snapshot", "Env", "Label", "Checkpoint"],
        &rows,
        profile.color,
    ));
    let warnings = removed
        .iter()
        .flat_map(|snapshot| {
            snapshot.warnings.iter().map(|warning| {
                KeyValueRow::warning("Warning", format!("{}: {warning}", snapshot.snapshot_id))
            })
        })
        .collect::<Vec<_>>();
    if !warnings.is_empty() {
        lines.extend(render_key_value_card("Warnings", &warnings, profile.color));
    }
    lines
}

fn env_snapshot_pruned_raw(removed: &[EnvSnapshotRemoveSummary]) -> Vec<String> {
    let mut lines = vec![format!("Pruned {} snapshot(s).", removed.len())];
    for snapshot in removed {
        let mut bits = vec![snapshot.snapshot_id.clone(), snapshot.env_name.clone()];
        if let Some(label) = snapshot.label.as_deref() {
            bits.push(format!("label={label}"));
        }
        bits.push(snapshot.archive_path.clone());
        lines.push(format!("  {}", bits.join("  ")));
        lines.extend(
            snapshot
                .warnings
                .iter()
                .map(|warning| format!("  warning: {}: {warning}", snapshot.snapshot_id)),
        );
    }
    lines
}

fn snapshot_binding_cell(snapshot: &EnvSnapshotSummary) -> Cell {
    if let Some(runtime) = snapshot.default_runtime.as_deref() {
        return Cell::accent(format!("runtime:{runtime}"));
    }
    if let Some(launcher) = snapshot.default_launcher.as_deref() {
        return Cell::accent(format!("launcher:{launcher}"));
    }
    Cell::muted("—")
}

pub fn env_resolved(summary: &ExecutionSummary, profile: RenderProfile) -> Vec<String> {
    if !profile.pretty {
        return env_resolved_raw(summary);
    }

    let mut lines = vec![paint(
        &format!("Execution plan {}", summary.env_name),
        Tone::Strong,
        profile.color,
    )];

    let mut resolution = vec![
        KeyValueRow::accent(
            "Binding",
            format!("{}:{}", summary.binding_kind, summary.binding_name),
        ),
        KeyValueRow::plain("Run dir", summary.run_dir.clone()),
    ];
    if let Some(command) = summary.command.as_ref() {
        resolution.push(KeyValueRow::accent("Command", command.clone()));
    }
    if let Some(binary_path) = summary.binary_path.as_ref() {
        resolution.push(KeyValueRow::accent("Binary", binary_path.clone()));
    }
    if let Some(source_kind) = summary.runtime_source_kind.as_ref() {
        resolution.push(KeyValueRow::plain("Runtime source", source_kind.clone()));
    }
    if let Some(release_version) = summary.runtime_release_version.as_ref() {
        resolution.push(KeyValueRow::plain(
            "Release version",
            release_version.clone(),
        ));
    }
    if let Some(release_channel) = summary.runtime_release_channel.as_ref() {
        resolution.push(KeyValueRow::plain(
            "Release channel",
            release_channel.clone(),
        ));
    }
    if !summary.forwarded_args.is_empty() {
        resolution.push(KeyValueRow::plain(
            "Forwarded args",
            summary.forwarded_args.join(" "),
        ));
    }
    push_card(&mut lines, "Resolution", resolution, profile.color);

    lines
}

fn env_resolved_raw(summary: &ExecutionSummary) -> Vec<String> {
    let mut lines = vec![
        format!("envName: {}", summary.env_name),
        format!("bindingKind: {}", summary.binding_kind),
        format!("bindingName: {}", summary.binding_name),
    ];
    if let Some(command) = summary.command.as_deref() {
        lines.push(format!("command: {command}"));
    }
    if let Some(binary_path) = summary.binary_path.as_deref() {
        lines.push(format!("binaryPath: {binary_path}"));
    }
    if let Some(source_kind) = summary.runtime_source_kind.as_deref() {
        lines.push(format!("runtimeSourceKind: {source_kind}"));
    }
    if let Some(release_version) = summary.runtime_release_version.as_deref() {
        lines.push(format!("runtimeReleaseVersion: {release_version}"));
    }
    if let Some(release_channel) = summary.runtime_release_channel.as_deref() {
        lines.push(format!("runtimeReleaseChannel: {release_channel}"));
    }
    if !summary.forwarded_args.is_empty() {
        lines.push(format!(
            "forwardedArgs: {}",
            summary.forwarded_args.join(" ")
        ));
    }
    lines.push(format!("runDir: {}", summary.run_dir));
    lines
}

pub fn env_runtime_updated(
    name: &str,
    runtime_name: Option<&str>,
    profile: RenderProfile,
) -> Vec<String> {
    env_binding_updated(name, "Runtime", "defaultRuntime", runtime_name, profile)
}

pub fn env_launcher_updated(
    name: &str,
    launcher_name: Option<&str>,
    profile: RenderProfile,
) -> Vec<String> {
    env_binding_updated(name, "Launcher", "defaultLauncher", launcher_name, profile)
}

/// A cleared binding reads differently from a bind, so it is never mistaken
/// for an entry named like the clear keyword.
fn env_binding_updated(
    name: &str,
    label: &str,
    field: &str,
    bound: Option<&str>,
    profile: RenderProfile,
) -> Vec<String> {
    if !profile.pretty {
        return vec![match bound {
            Some(bound) => format!("Updated env {name}: {field}={bound}"),
            None => format!("Updated env {name}: {field} cleared"),
        }];
    }

    let mut lines = vec![paint("Environment updated", Tone::Strong, profile.color)];
    push_card(
        &mut lines,
        "Binding",
        vec![
            KeyValueRow::accent("Env", name),
            match bound {
                Some(bound) => KeyValueRow::accent(label, bound),
                // Parentheses never appear in a registered name.
                None => KeyValueRow::muted(label, "(cleared)"),
            },
        ],
        profile.color,
    );
    lines
}
