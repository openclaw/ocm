use std::collections::BTreeMap;
use std::path::Path;

use crate::env::EnvMeta;
use crate::store::derive_env_paths;

pub const OPENCLAW_NATIVE_SERVICE_IDENTITY_KEYS: [&str; 3] = [
    "OPENCLAW_LAUNCHD_LABEL",
    "OPENCLAW_SYSTEMD_UNIT",
    "OPENCLAW_WINDOWS_TASK_NAME",
];

const OPENCLAW_SERVICE_MARKER: &str = "openclaw";
const OPENCLAW_GATEWAY_SERVICE_KIND: &str = "gateway";

pub fn quote_posix(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn quote_fish(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn render_assignment(shell: &str, key: &str, value: &str) -> String {
    if shell == "fish" {
        format!("set -gx {key} {};", quote_fish(value))
    } else {
        format!("export {key}={}", quote_posix(value))
    }
}

fn render_unset(shell: &str, key: &str) -> String {
    debug_assert!(is_shell_identifier(key));
    if shell == "fish" {
        format!("set -e {key};")
    } else {
        format!("unset {key}")
    }
}

fn is_shell_identifier(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some('_' | 'A'..='Z' | 'a'..='z'))
        && chars.all(|ch| matches!(ch, '_' | 'A'..='Z' | 'a'..='z' | '0'..='9'))
}

pub fn resolve_shell_name(explicit: Option<&str>, env: &BTreeMap<String, String>) -> String {
    if let Some(shell) = explicit {
        match shell.trim().to_ascii_lowercase().as_str() {
            "bash" | "fish" | "sh" | "zsh" => return shell.trim().to_ascii_lowercase(),
            _ => {}
        }
    }

    if env
        .get("SHELL")
        .map(|value| value.contains("fish"))
        .unwrap_or(false)
    {
        "fish".to_string()
    } else {
        "zsh".to_string()
    }
}

pub fn build_openclaw_env(
    meta: &EnvMeta,
    base_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let paths = derive_env_paths(Path::new(&meta.root));
    let mut next = sanitized_openclaw_base_env(base_env);
    next.insert(
        "OPENCLAW_HOME".to_string(),
        paths.openclaw_home.to_string_lossy().into_owned(),
    );
    next.insert(
        "OPENCLAW_STATE_DIR".to_string(),
        paths.state_dir.to_string_lossy().into_owned(),
    );
    next.insert(
        "OPENCLAW_CONFIG_PATH".to_string(),
        paths.config_path.to_string_lossy().into_owned(),
    );
    next.insert(
        "OPENCLAW_SERVICE_REPAIR_POLICY".to_string(),
        "external".to_string(),
    );
    if meta.service_enabled && meta.service_running {
        apply_external_supervision_hint(&mut next);
    }
    next.insert("OCM_ACTIVE_ENV".to_string(), meta.name.clone());
    next.insert(
        "OCM_ACTIVE_ENV_ROOT".to_string(),
        paths.root.to_string_lossy().into_owned(),
    );

    if let Some(port) = meta.gateway_port {
        next.insert("OPENCLAW_GATEWAY_PORT".to_string(), port.to_string());
    } else {
        next.remove("OPENCLAW_GATEWAY_PORT");
    }

    next.remove("OPENCLAW_PROFILE");
    next
}

pub fn apply_external_supervision_hint(env: &mut BTreeMap<String, String>) {
    scrub_native_service_identity(env);
    env.insert(
        "OPENCLAW_SERVICE_MARKER".to_string(),
        OPENCLAW_SERVICE_MARKER.to_string(),
    );
    env.remove("OPENCLAW_SERVICE_KIND");
    env.insert(
        "OPENCLAW_SUPERVISOR_MODE".to_string(),
        "external".to_string(),
    );
    env.insert("OPENCLAW_NO_RESPAWN".to_string(), "1".to_string());
}

pub fn apply_external_supervision_protocol_v1(env: &mut BTreeMap<String, String>) {
    apply_external_supervision_hint(env);
    env.insert(
        "OPENCLAW_SERVICE_KIND".to_string(),
        OPENCLAW_GATEWAY_SERVICE_KIND.to_string(),
    );
    env.remove("OPENCLAW_NO_RESPAWN");
}

pub fn apply_legacy_supervision(env: &mut BTreeMap<String, String>) {
    apply_external_supervision_hint(env);
    env.remove("OPENCLAW_SUPERVISOR_MODE");
}

fn scrub_native_service_identity(env: &mut BTreeMap<String, String>) {
    for key in OPENCLAW_NATIVE_SERVICE_IDENTITY_KEYS {
        env.remove(key);
    }
}

pub fn build_openclaw_dev_source_env(
    meta: &EnvMeta,
    base_env: &BTreeMap<String, String>,
    source_root: &Path,
) -> BTreeMap<String, String> {
    let mut next = build_openclaw_env(meta, base_env);
    let extensions_dir = source_root.join("extensions");
    if extensions_dir.is_dir() {
        next.insert(
            "OPENCLAW_DEV_SOURCE_ROOT".to_string(),
            source_root.to_string_lossy().into_owned(),
        );
        next.insert(
            "OPENCLAW_BUNDLED_PLUGINS_DIR".to_string(),
            extensions_dir.to_string_lossy().into_owned(),
        );
    }
    next
}

fn sanitized_openclaw_base_env(base_env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    base_env
        .iter()
        .filter(|(key, _)| {
            (!key.starts_with("OPENCLAW_") || is_openclaw_diagnostics_passthrough_key(key))
                && key.as_str() != "OCM_ACTIVE_ENV"
                && key.as_str() != "OCM_ACTIVE_ENV_ROOT"
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn is_openclaw_diagnostics_passthrough_key(key: &str) -> bool {
    matches!(
        key,
        "OPENCLAW_DIAGNOSTICS"
            | "OPENCLAW_DIAGNOSTICS_RUN_ID"
            | "OPENCLAW_DIAGNOSTICS_ENV"
            | "OPENCLAW_DIAGNOSTICS_TIMELINE_PATH"
            | "OPENCLAW_DIAGNOSTICS_EVENT_LOOP"
            | "OPENCLAW_GATEWAY_STARTUP_TRACE"
    )
}

pub fn render_use_script(
    meta: &EnvMeta,
    shell: &str,
    base_env: &BTreeMap<String, String>,
) -> String {
    let mut unset_keys = base_env
        .keys()
        .filter(|key| {
            is_shell_identifier(key)
                && ((key.starts_with("OPENCLAW_") && !is_openclaw_diagnostics_passthrough_key(key))
                    || matches!(key.as_str(), "OCM_ACTIVE_ENV" | "OCM_ACTIVE_ENV_ROOT"))
        })
        .cloned()
        .collect::<Vec<_>>();
    unset_keys.extend(
        [
            "OPENCLAW_PROFILE",
            "OPENCLAW_GATEWAY_PORT",
            "OPENCLAW_DEV_SOURCE_ROOT",
            "OPENCLAW_BUNDLED_PLUGINS_DIR",
            "OPENCLAW_SERVICE_MARKER",
            "OPENCLAW_SERVICE_KIND",
            "OPENCLAW_SUPERVISOR_MODE",
            "OPENCLAW_NO_RESPAWN",
            "OPENCLAW_LAUNCHD_LABEL",
            "OPENCLAW_SYSTEMD_UNIT",
            "OPENCLAW_WINDOWS_TASK_NAME",
        ]
        .into_iter()
        .map(str::to_string),
    );
    unset_keys.sort();
    unset_keys.dedup();

    let mut lines = unset_keys
        .into_iter()
        .map(|key| render_unset(shell, &key))
        .collect::<Vec<_>>();
    let target_env = build_openclaw_env(meta, base_env);
    for key in [
        "OPENCLAW_HOME",
        "OPENCLAW_STATE_DIR",
        "OPENCLAW_CONFIG_PATH",
        "OPENCLAW_SERVICE_REPAIR_POLICY",
        "OPENCLAW_SERVICE_MARKER",
        "OPENCLAW_SERVICE_KIND",
        "OPENCLAW_SUPERVISOR_MODE",
        "OPENCLAW_NO_RESPAWN",
        "OCM_ACTIVE_ENV",
        "OCM_ACTIVE_ENV_ROOT",
        "OPENCLAW_GATEWAY_PORT",
    ] {
        if let Some(value) = target_env.get(key) {
            lines.push(render_assignment(shell, key, value));
        }
    }

    format!("{}\n", lines.join("\n"))
}

fn render_init_posix(command: &str) -> String {
    let command = quote_posix(command);
    format!(
        "ocm_use() {{\n  script=\"$(command {command} env use \"$@\")\" || return $?\n  eval \"$script\"\n}}\n"
    )
}

fn render_init_fish(command: &str) -> String {
    let command = quote_fish(command);
    format!(
        "function ocm_use\n    set -l script (command {command} env use $argv)\n    or return $status\n    eval $script\nend\n"
    )
}

pub fn render_init_script(command: &str, shell: &str) -> Result<String, String> {
    match shell {
        "bash" | "sh" | "zsh" => Ok(render_init_posix(command)),
        "fish" => Ok(render_init_fish(command)),
        _ => Err(format!("unsupported shell: {shell}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::EnvMeta;
    use time::OffsetDateTime;

    #[test]
    fn build_openclaw_env_preserves_diagnostics_passthrough_only() {
        let meta = EnvMeta {
            upgrade_independent_paths: Vec::new(),
            kind: "ocm-env".to_string(),
            name: "demo".to_string(),
            root: "/tmp/ocm/envs/demo".to_string(),
            gateway_port: Some(19999),
            gateway_port_auto_assigned: false,
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            dev_ui_port: None,
            protected: false,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            last_used_at: None,
        };
        let base = BTreeMap::from([
            ("NODE_OPTIONS".to_string(), "--cpu-prof".to_string()),
            ("OPENCLAW_HOME".to_string(), "/tmp/wrong".to_string()),
            ("OPENCLAW_GATEWAY_PORT".to_string(), "12345".to_string()),
            ("OPENCLAW_PROFILE".to_string(), "wrong".to_string()),
            ("OPENCLAW_DIAGNOSTICS".to_string(), "timeline".to_string()),
            (
                "OPENCLAW_DIAGNOSTICS_TIMELINE_PATH".to_string(),
                "/tmp/kova/timeline.jsonl".to_string(),
            ),
            (
                "OPENCLAW_DIAGNOSTICS_EVENT_LOOP".to_string(),
                "1".to_string(),
            ),
            (
                "OPENCLAW_GATEWAY_STARTUP_TRACE".to_string(),
                "1".to_string(),
            ),
            (
                "OPENCLAW_RANDOM_USER_VALUE".to_string(),
                "strip-me".to_string(),
            ),
            (
                "OPENCLAW_LAUNCHD_LABEL".to_string(),
                "ai.openclaw.gateway".to_string(),
            ),
            ("OCM_ACTIVE_ENV".to_string(), "wrong".to_string()),
        ]);

        let env = build_openclaw_env(&meta, &base);

        assert_eq!(
            env.get("NODE_OPTIONS").map(String::as_str),
            Some("--cpu-prof")
        );
        assert_eq!(
            env.get("OPENCLAW_HOME").map(String::as_str),
            Some("/tmp/ocm/envs/demo")
        );
        assert_eq!(
            env.get("OPENCLAW_GATEWAY_PORT").map(String::as_str),
            Some("19999")
        );
        assert_eq!(
            env.get("OPENCLAW_DIAGNOSTICS").map(String::as_str),
            Some("timeline")
        );
        assert_eq!(
            env.get("OPENCLAW_DIAGNOSTICS_TIMELINE_PATH")
                .map(String::as_str),
            Some("/tmp/kova/timeline.jsonl")
        );
        assert_eq!(
            env.get("OPENCLAW_DIAGNOSTICS_EVENT_LOOP")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.get("OPENCLAW_GATEWAY_STARTUP_TRACE")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(env.get("OCM_ACTIVE_ENV").map(String::as_str), Some("demo"));
        assert_eq!(
            env.get("OPENCLAW_SERVICE_MARKER").map(String::as_str),
            Some("openclaw")
        );
        assert_eq!(
            env.get("OPENCLAW_SUPERVISOR_MODE").map(String::as_str),
            Some("external")
        );
        assert_eq!(
            env.get("OPENCLAW_NO_RESPAWN").map(String::as_str),
            Some("1")
        );
        assert!(!env.contains_key("OPENCLAW_SERVICE_KIND"));
        assert!(!env.contains_key("OPENCLAW_LAUNCHD_LABEL"));
        assert!(!env.contains_key("OPENCLAW_PROFILE"));
        assert!(!env.contains_key("OPENCLAW_RANDOM_USER_VALUE"));
    }

    #[test]
    fn build_openclaw_dev_source_env_points_bundled_plugins_at_source_extensions() {
        let meta = EnvMeta {
            upgrade_independent_paths: Vec::new(),
            kind: "ocm-env".to_string(),
            name: "demo".to_string(),
            root: "/tmp/ocm/envs/demo".to_string(),
            gateway_port: Some(19999),
            gateway_port_auto_assigned: false,
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            dev_ui_port: None,
            protected: false,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            last_used_at: None,
        };
        let root = std::env::temp_dir().join(format!("ocm-dev-source-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("extensions/codex")).unwrap();

        let env = build_openclaw_dev_source_env(&meta, &BTreeMap::new(), &root);

        assert_eq!(
            env.get("OPENCLAW_BUNDLED_PLUGINS_DIR").map(String::as_str),
            Some(root.join("extensions").to_string_lossy().as_ref())
        );
        assert_eq!(
            env.get("OPENCLAW_DEV_SOURCE_ROOT").map(String::as_str),
            Some(root.to_string_lossy().as_ref())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fish_quoting_preserves_backslashes_before_apostrophes() {
        assert_eq!(
            quote_fish(r"bad\'; touch /tmp/pwned; #"),
            r"'bad\\\'; touch /tmp/pwned; #'"
        );
    }

    #[test]
    fn activation_unsets_only_valid_stale_control_names() {
        let meta = EnvMeta {
            upgrade_independent_paths: Vec::new(),
            kind: "ocm-env".to_string(),
            name: "demo".to_string(),
            root: "/tmp/ocm/envs/demo".to_string(),
            gateway_port: None,
            gateway_port_auto_assigned: false,
            service_enabled: false,
            service_running: false,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            dev_ui_port: None,
            protected: false,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            last_used_at: None,
        };
        let base = BTreeMap::from([
            (
                "OPENCLAW_DEV_SOURCE_ROOT".to_string(),
                "/tmp/previous".to_string(),
            ),
            (
                "OPENCLAW_BUNDLED_PLUGINS_DIR".to_string(),
                "/tmp/previous/extensions".to_string(),
            ),
            (
                "OPENCLAW_RANDOM_USER_VALUE".to_string(),
                "stale".to_string(),
            ),
            ("OPENCLAW_DIAGNOSTICS".to_string(), "timeline".to_string()),
            (
                "OPENCLAW_X; touch /tmp/pwn".to_string(),
                "stale".to_string(),
            ),
        ]);

        let script = render_use_script(&meta, "zsh", &base);

        assert!(script.contains("unset OPENCLAW_DEV_SOURCE_ROOT"));
        assert!(script.contains("unset OPENCLAW_BUNDLED_PLUGINS_DIR"));
        assert!(script.contains("unset OPENCLAW_RANDOM_USER_VALUE"));
        assert!(script.contains("unset OPENCLAW_GATEWAY_PORT"));
        assert!(!script.contains("unset OPENCLAW_DIAGNOSTICS"));
        assert!(!script.contains("touch /tmp/pwn"));
        assert!(script.contains("export OPENCLAW_HOME='/tmp/ocm/envs/demo'"));
    }
}
