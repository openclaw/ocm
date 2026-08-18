use super::RenderProfile;
use crate::infra::terminal::{
    Cell, KeyValueRow, Tone, paint, render_key_value_card, render_table, terminal_width,
};
use crate::service::{
    ServiceActionSummary, ServiceInstallSummary, ServiceSummary, ServiceSummaryList,
};

fn ocm_service_state(installed: bool, loaded: bool, running: bool) -> &'static str {
    if running {
        "running"
    } else if loaded {
        "loaded"
    } else if installed {
        "installed"
    } else {
        "absent"
    }
}

fn service_state(summary: &ServiceSummary) -> &str {
    summary.gateway_state.as_str()
}

fn state_tone(state: &str) -> Tone {
    match state {
        "running" => Tone::Success,
        "pending" | "starting" | "loaded" | "installed" | "backoff" | "restarting" => Tone::Warning,
        "stopped" | "disabled" | "absent" => Tone::Muted,
        _ => Tone::Plain,
    }
}

fn optional_value_row(label: &str, value: Option<String>) -> KeyValueRow {
    match value {
        Some(value) => KeyValueRow::plain(label, value),
        None => KeyValueRow::muted(label, "—"),
    }
}

fn binding_label(summary: &ServiceSummary) -> String {
    match (
        summary.binding_kind.as_deref(),
        summary.binding_name.as_deref(),
    ) {
        (Some(kind), Some(name)) => format!("{kind}:{name}"),
        _ => "—".to_string(),
    }
}

fn gateway_state(summary: &ServiceSummary) -> &str {
    service_state(summary)
}

fn gateway_url(port: u32) -> String {
    format!("http://127.0.0.1:{port}")
}

fn ocm_background_service_state(summary: &ServiceSummary) -> &'static str {
    ocm_service_state(
        summary.ocm_service_installed,
        summary.ocm_service_loaded,
        summary.ocm_service_running,
    )
}

pub fn service_overview(summary: &ServiceSummaryList, profile: RenderProfile) -> Vec<String> {
    service_overview_with_width(summary, profile, terminal_width())
}

fn service_overview_with_width(
    summary: &ServiceSummaryList,
    profile: RenderProfile,
    _width: Option<usize>,
) -> Vec<String> {
    if !profile.pretty {
        return service_overview_raw(summary);
    }

    if summary.services.is_empty() {
        return vec!["No supervised env gateways.".to_string()];
    }

    let rows = summary
        .services
        .iter()
        .map(|service| {
            vec![
                Cell::accent(service.env_name.clone()),
                if binding_label(service) == "—" {
                    Cell::muted(binding_label(service))
                } else {
                    Cell::plain(binding_label(service))
                },
                Cell::right(service.gateway_port.to_string(), Tone::Accent),
                Cell::new(
                    gateway_state(service),
                    crate::infra::terminal::Align::Left,
                    state_tone(gateway_state(service)),
                ),
                Cell::new(
                    ocm_background_service_state(service),
                    crate::infra::terminal::Align::Left,
                    state_tone(ocm_background_service_state(service)),
                ),
            ]
        })
        .collect::<Vec<_>>();

    let mut lines = render_table(
        &["Env", "Binding", "Port", "Gateway", "OCM"],
        &rows,
        profile.color,
    );
    lines.push(String::new());
    lines.push(paint(
        &format!(
            "OCM background service: {}",
            ocm_service_state(
                summary.ocm_service_installed,
                summary.ocm_service_loaded,
                summary.ocm_service_running
            )
        ),
        Tone::Muted,
        profile.color,
    ));
    lines
}

fn service_overview_raw(summary: &ServiceSummaryList) -> Vec<String> {
    let mut lines = vec![format!(
        "ocmService state={}",
        ocm_service_state(
            summary.ocm_service_installed,
            summary.ocm_service_loaded,
            summary.ocm_service_running
        )
    )];
    for service in &summary.services {
        let mut bits = vec![
            service.env_name.clone(),
            format!("port={}", service.gateway_port),
            format!("gateway={}", gateway_state(service)),
            format!("ocmService={}", ocm_background_service_state(service)),
        ];
        if let (Some(kind), Some(name)) = (
            service.binding_kind.as_deref(),
            service.binding_name.as_deref(),
        ) {
            bits.insert(1, format!("binding={kind}:{name}"));
        }
        if let Some(issue) = service.issue.as_deref() {
            bits.push(format!("issue={issue}"));
        }
        lines.push(bits.join("  "));
    }
    lines
}

pub fn service_status(
    summary: &ServiceSummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return service_status_raw(summary);
    }

    let daemon = ocm_background_service_state(summary);
    let gateway = gateway_state(summary);

    let mut lines = vec![paint(
        &format!("Supervised env {}", summary.env_name),
        Tone::Strong,
        profile.color,
    )];

    let mut summary_rows = vec![
        KeyValueRow::new("Gateway", gateway, state_tone(gateway)),
        KeyValueRow::new("OCM service", daemon, state_tone(daemon)),
        KeyValueRow::plain("Binding", binding_label(summary)),
        KeyValueRow::accent("Port", summary.gateway_port.to_string()),
        KeyValueRow::plain("URL", gateway_url(summary.gateway_port)),
    ];
    if let Some(runtime) = compact_runtime_label(summary) {
        summary_rows.push(KeyValueRow::plain("Runtime", runtime));
    }
    if let Some(restart_handoff) = restart_handoff_label(summary) {
        summary_rows.push(KeyValueRow::plain("Restart handoff", restart_handoff));
    }
    lines.extend(render_key_value_card(
        "Status",
        &summary_rows,
        profile.color,
    ));

    if let Some(issue) = summary.issue.as_deref() {
        lines.extend(render_key_value_card(
            "Issue",
            &[
                KeyValueRow::warning("Status", issue.to_string()),
                optional_value_row("Detail", summary.last_error.clone()),
            ],
            profile.color,
        ));

        let details = service_detail_rows(summary);
        if !details.is_empty() {
            lines.extend(render_key_value_card("Details", &details, profile.color));
        }
        let logs = service_log_rows(summary);
        if !logs.is_empty() {
            lines.extend(render_key_value_card("Logs", &logs, profile.color));
        }
    } else if has_runtime_attention(summary) {
        let details = service_detail_rows(summary);
        if !details.is_empty() {
            lines.extend(render_key_value_card("Details", &details, profile.color));
        }
    }

    let next = service_status_next_steps(summary, command_example);
    if !next.is_empty() {
        lines.extend(render_key_value_card("Next", &next, profile.color));
    }

    lines
}

fn service_status_next_steps(summary: &ServiceSummary, command_example: &str) -> Vec<KeyValueRow> {
    if !summary.installed {
        return vec![
            KeyValueRow::accent(
                "Enable",
                format!("{command_example} service install {}", summary.env_name),
            ),
            KeyValueRow::plain(
                "Start",
                format!("{command_example} service start {}", summary.env_name),
            ),
        ];
    }

    if !summary.desired_running {
        return vec![KeyValueRow::accent(
            "Start",
            format!("{command_example} service start {}", summary.env_name),
        )];
    }

    if !summary.running {
        return vec![
            KeyValueRow::accent(
                "Restart",
                format!("{command_example} service restart {}", summary.env_name),
            ),
            KeyValueRow::plain("Inspect", format!("{command_example} service status")),
        ];
    }

    vec![
        KeyValueRow::accent(
            "Logs",
            format!("{command_example} logs {}", summary.env_name),
        ),
        KeyValueRow::plain(
            "Restart",
            format!("{command_example} service restart {}", summary.env_name),
        ),
    ]
}

fn service_status_raw(summary: &ServiceSummary) -> Vec<String> {
    let daemon = ocm_service_state(
        summary.ocm_service_installed,
        summary.ocm_service_loaded,
        summary.ocm_service_running,
    );
    vec![
        format!("envName: {}", summary.env_name),
        format!("serviceKind: {}", summary.service_kind),
        format!("binding: {}", binding_label(summary)),
        format!("gatewayPort: {}", summary.gateway_port),
        format!("installed: {}", summary.installed),
        format!("desiredRunning: {}", summary.desired_running),
        format!("running: {}", summary.running),
        format!("gatewayState: {}", gateway_state(summary)),
        format!(
            "restartHandoff: {}",
            summary
                .restart_handoff
                .clone()
                .unwrap_or_else(|| "unknown".to_string())
        ),
        format!("ocmService: {daemon}"),
        format!(
            "childPid: {}",
            summary
                .child_pid
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "stdoutPath: {}",
            summary
                .stdout_path
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "stderrPath: {}",
            summary
                .stderr_path
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "issue: {}",
            summary.issue.clone().unwrap_or_else(|| "none".to_string())
        ),
    ]
}

pub fn service_installed(
    summary: &ServiceInstallSummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    service_action(summary, profile, command_example)
}

pub fn service_action(
    summary: &ServiceActionSummary,
    profile: RenderProfile,
    command_example: &str,
) -> Vec<String> {
    if !profile.pretty {
        return service_action_raw(summary, command_example);
    }

    let state = summary.gateway_state.as_str();

    let mut lines = vec![paint(&action_title(summary), Tone::Strong, profile.color)];
    let mut result_rows = vec![
        KeyValueRow::plain("Action", summary.action.clone()),
        KeyValueRow::new("Gateway", state, state_tone(state)),
        KeyValueRow::accent("Port", summary.gateway_port.to_string()),
        optional_value_row(
            "Binding",
            summary
                .binding_kind
                .as_ref()
                .zip(summary.binding_name.as_ref())
                .map(|(kind, name)| format!("{kind}:{name}")),
        ),
    ];
    if let Some(ready) = summary.gateway_ready {
        result_rows.push(KeyValueRow::new(
            "Ready",
            if ready { "yes" } else { "no" },
            if ready { Tone::Success } else { Tone::Danger },
        ));
    }
    lines.extend(render_key_value_card("Result", &result_rows, profile.color));
    if let Some(issue) = summary.issue.as_ref() {
        lines.extend(render_key_value_card(
            "Attention",
            &[KeyValueRow::warning("Issue", issue.clone())],
            profile.color,
        ));
    }
    if !summary.warnings.is_empty() {
        let rows = summary
            .warnings
            .iter()
            .map(|warning| KeyValueRow::warning("Warning", warning.clone()))
            .collect::<Vec<_>>();
        lines.extend(render_key_value_card("Warnings", &rows, profile.color));
    }
    let next = service_action_next_steps(summary, command_example);
    if !next.is_empty() {
        lines.extend(render_key_value_card("Next", &next, profile.color));
    }
    lines
}

fn compact_runtime_label(summary: &ServiceSummary) -> Option<String> {
    match (
        summary.binding_kind.as_deref(),
        summary.binding_name.as_deref(),
        summary.runtime_release_version.as_deref(),
    ) {
        (Some("runtime"), Some(name), Some(version)) => Some(format!("{name} ({version})")),
        (Some("runtime"), Some(name), None) => Some(name.to_string()),
        _ => None,
    }
}

fn restart_handoff_label(summary: &ServiceSummary) -> Option<&'static str> {
    match summary.restart_handoff.as_deref() {
        Some("protocol-v1") => Some("protocol v1"),
        Some("legacy") => Some("legacy compatibility"),
        _ => None,
    }
}

fn has_runtime_attention(summary: &ServiceSummary) -> bool {
    summary.child_restart_count.unwrap_or_default() > 0
        || summary.last_exit_code.is_some()
        || summary.last_event_at.is_some()
        || summary.next_retry_at.is_some()
}

fn service_detail_rows(summary: &ServiceSummary) -> Vec<KeyValueRow> {
    let mut rows = Vec::new();
    if let Some(command) = summary.command.clone() {
        rows.push(KeyValueRow::plain("Command", command));
    }
    if let Some(binary) = summary.binary_path.clone() {
        rows.push(KeyValueRow::plain("Binary", binary));
    }
    rows.push(KeyValueRow::plain("Run dir", summary.run_dir.clone()));
    if let Some(pid) = summary.child_pid {
        rows.push(KeyValueRow::plain("Child pid", pid.to_string()));
    }
    if let Some(restart_count) = summary.child_restart_count
        && restart_count > 0
    {
        rows.push(KeyValueRow::warning(
            "Restart count",
            restart_count.to_string(),
        ));
    }
    if let Some(last_exit) = summary.last_exit_code {
        rows.push(KeyValueRow::plain("Last exit", last_exit.to_string()));
    }
    if let Some(last_event) = summary.last_event_at.clone() {
        rows.push(KeyValueRow::plain("Last event", last_event));
    }
    if let Some(next_retry) = summary.next_retry_at.clone() {
        rows.push(KeyValueRow::plain("Next retry", next_retry));
    }
    rows
}

fn service_log_rows(summary: &ServiceSummary) -> Vec<KeyValueRow> {
    let mut rows = Vec::new();
    if let Some(stdout) = summary.stdout_path.clone() {
        rows.push(KeyValueRow::plain("Stdout", stdout));
    }
    if let Some(stderr) = summary.stderr_path.clone() {
        rows.push(KeyValueRow::plain("Stderr", stderr));
    }
    rows
}

fn service_action_next_steps(
    summary: &ServiceActionSummary,
    command_example: &str,
) -> Vec<KeyValueRow> {
    if summary.running {
        return vec![
            KeyValueRow::accent(
                "Logs",
                format!("{command_example} logs {}", summary.env_name),
            ),
            KeyValueRow::plain(
                "Status",
                format!("{command_example} service status {}", summary.env_name),
            ),
        ];
    }
    if summary.installed {
        return vec![KeyValueRow::accent(
            "Start",
            format!("{command_example} service start {}", summary.env_name),
        )];
    }
    Vec::new()
}

fn service_action_raw(summary: &ServiceActionSummary, _command_example: &str) -> Vec<String> {
    let mut lines = vec![
        format!("envName: {}", summary.env_name),
        format!("action: {}", summary.action),
        format!("installed: {}", summary.installed),
        format!("desiredRunning: {}", summary.desired_running),
        format!("running: {}", summary.running),
        format!("gatewayPort: {}", summary.gateway_port),
        format!("gatewayState: {}", summary.gateway_state),
        format!(
            "gatewayReady: {}",
            summary
                .gateway_ready
                .map(|ready| ready.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "issue: {}",
            summary.issue.clone().unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "stdoutPath: {}",
            summary
                .stdout_path
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "stderrPath: {}",
            summary
                .stderr_path
                .clone()
                .unwrap_or_else(|| "none".to_string())
        ),
    ];
    lines.extend(
        summary
            .warnings
            .iter()
            .map(|warning| format!("warning: {warning}")),
    );
    lines
}

fn action_title(summary: &ServiceActionSummary) -> String {
    if summary.gateway_ready == Some(false) {
        return format!("Gateway for {} is not ready", summary.env_name);
    }
    match summary.action.as_str() {
        "install" => format!("Enabled {} in the OCM background service", summary.env_name),
        "start" => format!(
            "Started {} under the OCM background service",
            summary.env_name
        ),
        "stop" => format!(
            "Stopped {} under the OCM background service",
            summary.env_name
        ),
        "restart" => format!(
            "Restarted {} under the OCM background service",
            summary.env_name
        ),
        "uninstall" => format!(
            "Disabled {} in the OCM background service",
            summary.env_name
        ),
        _ => format!("Updated {} in the OCM background service", summary.env_name),
    }
}

#[cfg(test)]
mod tests {
    use super::{RenderProfile, service_action, service_overview, service_status};
    use crate::service::{ServiceActionSummary, ServiceSummary, ServiceSummaryList};

    fn sample_service() -> ServiceSummary {
        ServiceSummary {
            env_name: "demo".to_string(),
            service_kind: "gateway".to_string(),
            binding_kind: Some("runtime".to_string()),
            binding_name: Some("stable".to_string()),
            command: None,
            binary_path: Some("/tmp/openclaw".to_string()),
            runtime_source_kind: Some("official".to_string()),
            runtime_release_version: Some("2026.04.01".to_string()),
            runtime_release_channel: Some("stable".to_string()),
            args: vec!["gateway".to_string()],
            run_dir: "/tmp/demo".to_string(),
            gateway_port: 18789,
            installed: true,
            loaded: true,
            running: true,
            gateway_state: "running".to_string(),
            restart_handoff: Some("protocol-v1".to_string()),
            desired_running: true,
            ocm_service_installed: true,
            ocm_service_loaded: true,
            ocm_service_running: true,
            ocm_service_pid: Some(42),
            ocm_service_state: Some("running".to_string()),
            child_pid: Some(99),
            child_restart_count: Some(0),
            child_port: Some(18789),
            last_exit_code: None,
            last_error: None,
            last_event_at: None,
            next_retry_at: None,
            stdout_path: Some("/tmp/stdout.log".to_string()),
            stderr_path: Some("/tmp/stderr.log".to_string()),
            issue: None,
        }
    }

    #[test]
    fn service_overview_pretty_renders_table() {
        let lines = service_overview(
            &ServiceSummaryList {
                ocm_service_label: "ocm.ocm".to_string(),
                ocm_service_installed: true,
                ocm_service_loaded: true,
                ocm_service_running: true,
                ocm_service_pid: Some(42),
                ocm_service_state: Some("running".to_string()),
                services: vec![sample_service()],
            },
            RenderProfile::pretty(false),
        );
        assert!(lines.iter().any(|line| line.contains("demo")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("OCM background service"))
        );
    }

    #[test]
    fn service_status_pretty_shows_logs_next_step() {
        let lines = service_status(&sample_service(), RenderProfile::pretty(false), "ocm");
        assert!(lines.iter().any(|line| line.contains("logs demo")));
    }

    #[test]
    fn service_status_pretty_hides_debug_details_when_healthy() {
        let lines = service_status(&sample_service(), RenderProfile::pretty(false), "ocm");
        assert!(lines.iter().any(|line| line.contains("Runtime")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("http://127.0.0.1:18789"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Restart handoff") && line.contains("protocol v1"))
        );
        assert!(!lines.iter().any(|line| line.contains("/tmp/openclaw")));
        assert!(!lines.iter().any(|line| line.contains("/tmp/stdout.log")));
        assert!(!lines.iter().any(|line| line.contains("Child pid")));
        assert!(!lines.iter().any(|line| line.contains("Restart count")));
    }

    #[test]
    fn service_action_pretty_hides_log_paths() {
        let lines = service_action(
            &ServiceActionSummary {
                env_name: "demo".to_string(),
                service_kind: "gateway".to_string(),
                action: "restart".to_string(),
                installed: true,
                loaded: true,
                desired_running: true,
                running: true,
                gateway_port: 18789,
                gateway_state: "running".to_string(),
                gateway_ready: Some(true),
                issue: None,
                binding_kind: Some("runtime".to_string()),
                binding_name: Some("stable".to_string()),
                stdout_path: Some("/tmp/stdout.log".to_string()),
                stderr_path: Some("/tmp/stderr.log".to_string()),
                warnings: Vec::new(),
            },
            RenderProfile::pretty(false),
            "ocm",
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("service status demo"))
        );
        assert!(!lines.iter().any(|line| line.contains("/tmp/stdout.log")));
        assert!(!lines.iter().any(|line| line.contains("/tmp/stderr.log")));
    }

    #[test]
    fn service_action_raw_preserves_operational_warnings() {
        let mut summary = ServiceActionSummary {
            env_name: "demo".to_string(),
            service_kind: "gateway".to_string(),
            action: "stop".to_string(),
            installed: true,
            loaded: true,
            desired_running: false,
            running: true,
            gateway_port: 18789,
            gateway_state: "stopping".to_string(),
            gateway_ready: None,
            issue: None,
            binding_kind: Some("runtime".to_string()),
            binding_name: Some("stable".to_string()),
            stdout_path: None,
            stderr_path: None,
            warnings: vec!["gateway is still shutting down".to_string()],
        };
        let lines = service_action(&summary, RenderProfile::raw(), "ocm");
        assert!(
            lines
                .iter()
                .any(|line| line == "warning: gateway is still shutting down")
        );

        summary.warnings.clear();
        let lines = service_action(&summary, RenderProfile::raw(), "ocm");
        assert!(!lines.iter().any(|line| line.starts_with("warning:")));
    }
}
