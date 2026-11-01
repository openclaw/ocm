pub mod doctor;
pub mod env;
pub mod help;
pub mod launcher;
pub mod logs;
pub mod migrate;
pub mod release;
pub mod runtime;
pub mod self_update;
pub mod service;
pub mod upgrade;

use std::collections::BTreeMap;

use time::OffsetDateTime;

use crate::infra::terminal::{KeyValueRow, render_key_value_card};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderProfile {
    pub pretty: bool,
    pub color: bool,
}

impl RenderProfile {
    pub fn raw() -> Self {
        Self {
            pretty: false,
            color: false,
        }
    }

    pub fn pretty(color: bool) -> Self {
        Self {
            pretty: true,
            color,
        }
    }
}

pub(super) fn format_key_value_lines(lines: BTreeMap<String, String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|(key, value)| format!("{key}: {value}"))
        .collect()
}

pub(crate) fn format_rfc3339(value: OffsetDateTime) -> Result<String, String> {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| error.to_string())
}

pub(super) fn push_card(lines: &mut Vec<String>, title: &str, rows: Vec<KeyValueRow>, color: bool) {
    if rows.is_empty() {
        return;
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend(render_key_value_card(title, &rows, color));
}

pub(super) fn optional_value_row(label: &str, value: Option<String>) -> KeyValueRow {
    match value {
        Some(value) => KeyValueRow::plain(label, value),
        None => KeyValueRow::muted(label, "—"),
    }
}
