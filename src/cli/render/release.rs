use std::collections::BTreeMap;

use crate::infra::terminal::{Cell, KeyValueRow, Tone, paint, render_table, terminal_width};
use crate::runtime::OpenClawReleaseCatalogEntry;

use super::{RenderProfile, format_key_value_lines, format_rfc3339, optional_value_row, push_card};

pub fn release_list(
    releases: &[OpenClawReleaseCatalogEntry],
    profile: RenderProfile,
) -> Vec<String> {
    release_list_with_width(releases, profile, terminal_width())
}

fn release_list_with_width(
    releases: &[OpenClawReleaseCatalogEntry],
    profile: RenderProfile,
    width: Option<usize>,
) -> Vec<String> {
    if releases.is_empty() {
        return vec![paint(
            "No published OpenClaw releases.",
            Tone::Muted,
            profile.color,
        )];
    }
    if !profile.pretty {
        return release_list_raw(releases);
    }

    let show_full = width.map(|value| value >= 110).unwrap_or(true);
    let rows = releases
        .iter()
        .map(|release| {
            let published_at = release
                .release
                .published_at
                .map(format_rfc3339)
                .transpose()
                .unwrap_or_else(|_| Some("—".to_string()))
                .unwrap_or_else(|| "—".to_string());
            let installed = if release.installed_runtime_names.is_empty() {
                "—".to_string()
            } else {
                release.installed_runtime_names.join(", ")
            };
            let install_as = release_install_name(release);
            if show_full {
                vec![
                    Cell::accent(release.release.version.clone()),
                    optional_cell(release.release.channel.as_deref()),
                    Cell::accent(install_as),
                    Cell::muted(published_at),
                    Cell::plain(installed),
                    Cell::muted(release.release.tarball_url.clone()),
                ]
            } else {
                vec![
                    Cell::accent(release.release.version.clone()),
                    optional_cell(release.release.channel.as_deref()),
                    Cell::accent(install_as),
                    Cell::muted(published_at),
                    Cell::plain(installed),
                ]
            }
        })
        .collect::<Vec<_>>();

    let mut lines = render_table(
        if show_full {
            &[
                "Version",
                "Channel",
                "Install as",
                "Published",
                "Installed",
                "Tarball",
            ]
        } else {
            &["Version", "Channel", "Install as", "Published", "Installed"]
        },
        &rows,
        profile.color,
    );
    if !show_full {
        lines.push(String::new());
        lines.push(paint(
            "Use release show <version>, --raw, or --json for tarball details.",
            Tone::Muted,
            profile.color,
        ));
    }
    lines
}

fn release_list_raw(releases: &[OpenClawReleaseCatalogEntry]) -> Vec<String> {
    let mut lines = Vec::with_capacity(releases.len());
    for release in releases {
        let mut bits = vec![
            release.release.version.clone(),
            release.release.tarball_url.clone(),
        ];
        if let Some(channel) = release.release.channel.as_deref() {
            bits.push(format!("channel={channel}"));
        }
        bits.push(format!("installAs={}", release_install_name(release)));
        if let Some(published_at) = release.release.published_at
            && let Ok(published_at) = format_rfc3339(published_at)
        {
            bits.push(format!("publishedAt={published_at}"));
        }
        if let Some(shasum) = release.release.shasum.as_deref() {
            bits.push(format!("shasum={shasum}"));
        }
        if let Some(integrity) = release.release.integrity.as_deref() {
            bits.push(format!("integrity={integrity}"));
        }
        if !release.installed_runtime_names.is_empty() {
            bits.push(format!(
                "installed={}",
                release.installed_runtime_names.join(",")
            ));
        }
        lines.push(bits.join("  "));
    }
    lines
}

fn optional_cell(value: Option<&str>) -> Cell {
    value.map(Cell::plain).unwrap_or_else(|| Cell::muted("—"))
}

fn release_install_name(release: &OpenClawReleaseCatalogEntry) -> String {
    release
        .release
        .channel
        .clone()
        .unwrap_or_else(|| release.release.version.clone())
}

pub fn release_show(
    release: &OpenClawReleaseCatalogEntry,
    profile: RenderProfile,
    command_example: &str,
    install_channel: Option<&str>,
) -> Result<Vec<String>, String> {
    if !profile.pretty {
        return release_show_raw(release);
    }

    let mut lines = vec![paint(
        &format!("Release {}", release.release.version),
        Tone::Strong,
        profile.color,
    )];

    push_card(
        &mut lines,
        "Published release",
        vec![
            KeyValueRow::accent("Version", release.release.version.clone()),
            optional_value_row("Channel", release.release.channel.clone()),
            optional_value_row(
                "Published",
                release
                    .release
                    .published_at
                    .map(format_rfc3339)
                    .transpose()?,
            ),
        ],
        profile.color,
    );

    let installed_runtimes = if release.installed_runtime_names.is_empty() {
        "none".to_string()
    } else {
        release.installed_runtime_names.join(", ")
    };
    push_card(
        &mut lines,
        "Local runtime mapping",
        vec![
            KeyValueRow::accent("Install name", release_install_name(release)),
            KeyValueRow::new(
                "Installed",
                installed_runtimes,
                if release.installed_runtime_names.is_empty() {
                    Tone::Muted
                } else {
                    Tone::Success
                },
            ),
        ],
        profile.color,
    );

    push_card(
        &mut lines,
        "Package",
        vec![
            KeyValueRow::accent("Tarball", release.release.tarball_url.clone()),
            optional_value_row("Shasum", release.release.shasum.clone()),
            optional_value_row("Integrity", release.release.integrity.clone()),
        ],
        profile.color,
    );

    let next_steps = release_show_next_steps(release, command_example, install_channel);
    if !next_steps.is_empty() {
        push_card(&mut lines, "Next", next_steps, profile.color);
    }

    Ok(lines)
}

fn release_show_next_steps(
    release: &OpenClawReleaseCatalogEntry,
    command_example: &str,
    install_channel: Option<&str>,
) -> Vec<KeyValueRow> {
    let install_command = match install_channel {
        Some(channel) => format!("{command_example} release install --channel {channel}"),
        None => format!(
            "{command_example} release install --version {}",
            release.release.version
        ),
    };

    let mut rows = vec![KeyValueRow::accent("Install", install_command)];
    if let Some(runtime_name) = release.installed_runtime_names.first() {
        rows.push(KeyValueRow::accent(
            "Inspect runtime",
            format!("{command_example} runtime show {runtime_name}"),
        ));
    }
    rows
}

fn release_show_raw(release: &OpenClawReleaseCatalogEntry) -> Result<Vec<String>, String> {
    let mut lines = BTreeMap::new();
    lines.insert("version".to_string(), release.release.version.clone());
    if let Some(channel) = release.release.channel.as_deref() {
        lines.insert("channel".to_string(), channel.to_string());
    }
    lines.insert(
        "tarballUrl".to_string(),
        release.release.tarball_url.clone(),
    );
    if let Some(published_at) = release.release.published_at {
        lines.insert("publishedAt".to_string(), format_rfc3339(published_at)?);
    }
    if let Some(shasum) = release.release.shasum.as_deref() {
        lines.insert("shasum".to_string(), shasum.to_string());
    }
    if let Some(integrity) = release.release.integrity.as_deref() {
        lines.insert("integrity".to_string(), integrity.to_string());
    }
    if release.installed_runtime_names.is_empty() {
        lines.insert("installedRuntimes".to_string(), "—".to_string());
    } else {
        lines.insert(
            "installedRuntimes".to_string(),
            release.installed_runtime_names.join(", "),
        );
    }
    Ok(format_key_value_lines(lines))
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::{RenderProfile, release_list_raw, release_list_with_width, release_show};
    use crate::runtime::{OpenClawRelease, OpenClawReleaseCatalogEntry};

    #[test]
    fn release_list_pretty_compacts_on_narrow_terminals() {
        let lines = release_list_with_width(
            &[sample_catalog_entry()],
            RenderProfile::pretty(false),
            Some(80),
        );

        assert!(lines[1].contains("Version"));
        assert!(lines[1].contains("Install as"));
        assert!(lines[1].contains("Installed"));
        assert!(!lines[1].contains("Tarball"));
        assert_eq!(
            lines.last().unwrap(),
            "Use release show <version>, --raw, or --json for tarball details."
        );
    }

    #[test]
    fn release_list_raw_includes_install_name() {
        let lines = release_list_raw(&[sample_catalog_entry()]);

        assert!(lines[0].contains("installAs=stable"));
    }

    #[test]
    fn release_show_pretty_uses_cards() {
        let lines = release_show(
            &sample_catalog_entry(),
            RenderProfile::pretty(false),
            "ocm",
            None,
        )
        .unwrap();

        assert_eq!(lines[0], "Release 2026.3.24");
        assert!(lines.iter().any(|line| line.contains("Published release")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Local runtime mapping"))
        );
        assert!(lines.iter().any(|line| line.contains("Install name")));
        assert!(lines.iter().any(|line| line.contains("stable")));
        assert!(lines.iter().any(|line| line.contains("Package")));
    }

    #[test]
    fn release_show_pretty_pins_version_next_step() {
        let lines = release_show(
            &sample_catalog_entry(),
            RenderProfile::pretty(false),
            "ocm",
            None,
        )
        .unwrap();

        assert!(lines.iter().any(|line| line.contains("Next")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm release install --version 2026.3.24"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm runtime show stable"))
        );
    }

    #[test]
    fn release_show_pretty_preserves_channel_next_step() {
        let lines = release_show(
            &sample_catalog_entry(),
            RenderProfile::pretty(false),
            "ocm",
            Some("stable"),
        )
        .unwrap();

        assert!(
            lines
                .iter()
                .any(|line| line.contains("ocm release install --channel stable"))
        );
    }

    #[test]
    fn release_show_raw_keeps_key_value_lines() {
        let lines =
            release_show(&sample_catalog_entry(), RenderProfile::raw(), "ocm", None).unwrap();

        assert!(lines.iter().any(|line| line == "version: 2026.3.24"));
        assert!(lines.iter().any(|line| line == "channel: stable"));
        assert!(lines.iter().any(|line| line.starts_with("tarballUrl: ")));
        assert!(lines.iter().any(|line| line == "installedRuntimes: stable"));
        assert!(!lines.iter().any(|line| line.contains('┌')));
    }

    fn sample_release() -> OpenClawRelease {
        OpenClawRelease {
            version: "2026.3.24".to_string(),
            channel: Some("stable".to_string()),
            channels: vec!["stable".to_string()],
            tarball_url: "https://registry.npmjs.org/openclaw/-/openclaw-2026.3.24.tgz".to_string(),
            shasum: Some("abc123".to_string()),
            integrity: Some("sha512-demo".to_string()),
            published_at: Some(OffsetDateTime::UNIX_EPOCH),
        }
    }

    fn sample_catalog_entry() -> OpenClawReleaseCatalogEntry {
        OpenClawReleaseCatalogEntry {
            release: sample_release(),
            installed_runtime_names: vec!["stable".to_string()],
        }
    }
}
