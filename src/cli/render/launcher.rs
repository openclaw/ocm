use std::collections::BTreeMap;

use crate::infra::terminal::{Cell, KeyValueRow, Tone, paint, render_table, terminal_width};
use crate::launcher::LauncherMeta;

use super::{RenderProfile, format_key_value_lines, format_rfc3339, push_card};

pub fn launcher_added(
    meta: &LauncherMeta,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return launcher_added_raw(meta);
    }

    let mut lines = vec![paint("Launcher added", Tone::Strong, profile.color)];
    let mut rows = vec![
        KeyValueRow::accent("Name", meta.name.clone()),
        KeyValueRow::plain("Command", meta.command.clone()),
    ];
    if let Some(cwd) = meta.cwd.as_deref() {
        rows.push(KeyValueRow::plain("Cwd", cwd));
    }
    if let Some(description) = meta.description.as_deref() {
        rows.push(KeyValueRow::plain("Description", description));
    }
    push_card(&mut lines, "Launcher", rows, profile.color);
    push_card(
        &mut lines,
        "Next",
        vec![
            KeyValueRow::accent(
                "Use in env",
                format!("{command_example} env create mira --launcher {}", meta.name),
            ),
            KeyValueRow::accent(
                "Show",
                format!("{command_example} launcher show {}", meta.name),
            ),
        ],
        profile.color,
    );
    lines
}

fn launcher_added_raw(meta: &LauncherMeta) -> Vec<String> {
    let mut lines = vec![
        format!("Added launcher {}", meta.name),
        format!("  command: {}", meta.command),
    ];
    if let Some(cwd) = meta.cwd.as_deref() {
        lines.push(format!("  cwd: {cwd}"));
    }
    lines
}

pub fn launcher_list(launchers: &[LauncherMeta], profile: RenderProfile) -> Vec<String> {
    launcher_list_with_width(launchers, profile, terminal_width())
}

fn launcher_list_with_width(
    launchers: &[LauncherMeta],
    profile: RenderProfile,
    width: Option<usize>,
) -> Vec<String> {
    if launchers.is_empty() {
        return vec!["No launchers.".to_string()];
    }
    if !profile.pretty {
        return launcher_list_raw(launchers);
    }

    let show_cwd = width.map(|width| width >= 100).unwrap_or(true);
    let rows = launchers
        .iter()
        .map(|meta| {
            let mut row = vec![
                Cell::accent(meta.name.clone()),
                Cell::plain(meta.command.clone()),
            ];
            if show_cwd {
                row.push(
                    meta.cwd
                        .as_deref()
                        .map(Cell::muted)
                        .unwrap_or_else(|| Cell::muted("—")),
                );
            }
            row
        })
        .collect::<Vec<_>>();
    let mut lines = render_table(
        if show_cwd {
            &["Name", "Command", "Cwd"]
        } else {
            &["Name", "Command"]
        },
        &rows,
        profile.color,
    );
    if !show_cwd {
        lines.push(String::new());
        lines.push(paint(
            "Use --raw for full cwd details.",
            Tone::Muted,
            profile.color,
        ));
    }
    lines
}

fn launcher_list_raw(launchers: &[LauncherMeta]) -> Vec<String> {
    let mut lines = Vec::with_capacity(launchers.len());
    for meta in launchers {
        let mut bits = vec![meta.name.clone(), meta.command.clone()];
        if let Some(cwd) = meta.cwd.as_deref() {
            bits.push(format!("cwd={cwd}"));
        }
        lines.push(bits.join("  "));
    }
    lines
}

pub fn launcher_show(
    meta: &LauncherMeta,
    profile: RenderProfile,
    command_example: &str,
) -> Result<Vec<String>, String> {
    if !profile.pretty {
        return launcher_show_raw(meta);
    }

    let mut lines = vec![paint(
        &format!("Launcher {}", meta.name),
        Tone::Strong,
        profile.color,
    )];
    let mut rows = vec![
        KeyValueRow::accent("Name", meta.name.clone()),
        KeyValueRow::plain("Command", meta.command.clone()),
    ];
    if let Some(cwd) = meta.cwd.as_deref() {
        rows.push(KeyValueRow::plain("Cwd", cwd));
    }
    if let Some(description) = meta.description.as_deref() {
        rows.push(KeyValueRow::plain("Description", description));
    }
    push_card(&mut lines, "Launcher", rows, profile.color);
    push_card(
        &mut lines,
        "Next",
        vec![
            KeyValueRow::accent(
                "Use in env",
                format!("{command_example} env create mira --launcher {}", meta.name),
            ),
            KeyValueRow::warning(
                "Remove",
                format!("{command_example} launcher remove {}", meta.name),
            ),
        ],
        profile.color,
    );
    Ok(lines)
}

fn launcher_show_raw(meta: &LauncherMeta) -> Result<Vec<String>, String> {
    let mut lines = BTreeMap::new();
    lines.insert("kind".to_string(), meta.kind.clone());
    lines.insert("name".to_string(), meta.name.clone());
    lines.insert("command".to_string(), meta.command.clone());
    lines.insert("createdAt".to_string(), format_rfc3339(meta.created_at)?);
    lines.insert("updatedAt".to_string(), format_rfc3339(meta.updated_at)?);
    if let Some(cwd) = meta.cwd.as_deref() {
        lines.insert("cwd".to_string(), cwd.to_string());
    }
    if let Some(description) = meta.description.as_deref() {
        lines.insert("description".to_string(), description.to_string());
    }
    Ok(format_key_value_lines(lines))
}

pub fn launcher_removed(name: &str, profile: RenderProfile, command_example: &str) -> Vec<String> {
    if !profile.pretty {
        return vec![format!("Removed launcher {name}")];
    }

    let mut lines = vec![paint("Launcher removed", Tone::Strong, profile.color)];
    push_card(
        &mut lines,
        "Launcher",
        vec![KeyValueRow::accent("Name", name)],
        profile.color,
    );
    push_card(
        &mut lines,
        "Next",
        vec![KeyValueRow::accent(
            "List",
            format!("{command_example} launcher list"),
        )],
        profile.color,
    );
    lines
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::{RenderProfile, launcher_list_with_width};
    use crate::launcher::LauncherMeta;

    #[test]
    fn launcher_list_pretty_compacts_on_narrow_terminals() {
        let lines =
            launcher_list_with_width(&[sample_launcher()], RenderProfile::pretty(false), Some(80));

        assert!(lines[1].contains("Command"));
        assert!(!lines[1].contains("Cwd"));
        assert_eq!(lines.last().unwrap(), "Use --raw for full cwd details.");
    }

    #[test]
    fn launcher_list_pretty_keeps_cwd_on_wide_terminals() {
        let lines = launcher_list_with_width(
            &[sample_launcher()],
            RenderProfile::pretty(false),
            Some(140),
        );

        assert!(lines[1].contains("Cwd"));
        assert!(lines.iter().any(|line| line.contains("/tmp/openclaw")));
    }

    fn sample_launcher() -> LauncherMeta {
        LauncherMeta {
            kind: "ocm-launcher".to_string(),
            name: "dev".to_string(),
            command: "pnpm openclaw".to_string(),
            cwd: Some("/tmp/openclaw".to_string()),
            description: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }
}
