use std::collections::BTreeMap;

use crate::cli::self_cmd::{SelfUpdateMode, SelfUpdateStatus, SelfUpdateSummary};
use crate::infra::terminal::{KeyValueRow, render_key_value_card};

use super::{RenderProfile, format_key_value_lines};

pub fn self_update(summary: &SelfUpdateSummary, profile: RenderProfile, cmd: &str) -> Vec<String> {
    if !profile.pretty {
        return self_update_raw(summary);
    }

    let mut lines = vec![format!(
        "OCM {}",
        match summary.mode {
            SelfUpdateMode::Check => "update check",
            SelfUpdateMode::Update => "self update",
        }
    )];

    let status = match summary.status {
        SelfUpdateStatus::UpToDate => "up to date",
        SelfUpdateStatus::UpdateAvailable => "update available",
        SelfUpdateStatus::Updated => "updated",
    };

    push_card(
        &mut lines,
        "Status",
        vec![
            KeyValueRow::plain("Current", summary.current_version.clone()),
            KeyValueRow::accent("Target", summary.target_version.clone()),
            KeyValueRow::success("Result", status),
        ],
        profile.color,
    );
    push_card(
        &mut lines,
        "Binary",
        vec![
            KeyValueRow::plain("Path", summary.binary_path.clone()),
            KeyValueRow::plain("Asset", summary.asset_name.clone()),
        ],
        profile.color,
    );

    let mut next = Vec::new();
    if let Some(path) = &summary.receipt_path {
        next.push(KeyValueRow::plain("Receipt", path.clone()));
    }
    if matches!(summary.status, SelfUpdateStatus::UpdateAvailable) {
        next.push(KeyValueRow::accent(
            "Install",
            if summary.target_version == "latest" {
                format!("{cmd} self update")
            } else {
                format!("{cmd} self update --version {}", summary.target_version)
            },
        ));
    } else if matches!(summary.status, SelfUpdateStatus::Updated) {
        next.push(KeyValueRow::accent("Run", format!("{cmd} --version")));
        if summary.daemon_refresh_required {
            next.push(KeyValueRow::warning(
                "Refresh daemon",
                format!("{cmd} service refresh-daemon --acknowledge-gateway-restarts"),
            ));
        }
    }
    if let Some(note) = summary.daemon_refresh_note.as_deref() {
        next.push(KeyValueRow::warning("Background service", note.to_string()));
    }
    if !next.is_empty() {
        push_card(&mut lines, "Next", next, profile.color);
    }

    lines
}

fn self_update_raw(summary: &SelfUpdateSummary) -> Vec<String> {
    let mut lines = BTreeMap::new();
    lines.insert("mode".to_string(), summary.mode.as_str().to_string());
    lines.insert("status".to_string(), summary.status.as_str().to_string());
    lines.insert(
        "currentVersion".to_string(),
        summary.current_version.clone(),
    );
    lines.insert("targetVersion".to_string(), summary.target_version.clone());
    lines.insert("binaryPath".to_string(), summary.binary_path.clone());
    lines.insert("assetName".to_string(), summary.asset_name.clone());
    lines.insert(
        "daemonRefreshRequired".to_string(),
        summary.daemon_refresh_required.to_string(),
    );
    if let Some(note) = summary.daemon_refresh_note.as_ref() {
        lines.insert("daemonRefreshNote".to_string(), note.clone());
    }
    if let Some(path) = &summary.receipt_path {
        lines.insert("receiptPath".to_string(), path.clone());
    }
    format_key_value_lines(lines)
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
