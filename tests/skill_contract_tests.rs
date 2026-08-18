use std::fs;

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

const RELEASE_VALIDATION_SUBSYSTEMS: [&str; 19] = [
    "Pairing",
    "Channels",
    "Control UI",
    "TUI",
    "Onboarding",
    "Slash commands",
    "Memory",
    "Subagents",
    "Agents",
    "Cron",
    "Sessions",
    "Context Engine",
    "Skill Workshop",
    "MCP",
    "Models",
    "Approvals",
    "Compaction",
    "Codex harness",
    "OpenClaw harness",
];

#[test]
fn release_validation_is_manual_and_worksheet_driven() {
    let skill = read("skills/openclaw-release-validation/SKILL.md");
    let worksheet = read("skills/openclaw-release-validation/assets/validation-worksheet.md");
    let normalized = normalize(&skill);

    for required in [
        "disable-model-invocation: true",
        "one editable markdown worksheet",
        "finish validation",
        "three to five subsystems",
        "group every user-visible or upgrade-sensitive item",
        "priority for this release",
        "other subsystems",
        "`#### what changed`",
        "`#### recommended testing`",
        "`#### notes`",
        "every priority subsystem must include it",
        "no notable changes in this release.",
        "omit **recommended testing**",
        "dominant themes across the subsystem's complete group",
        "do not include issue, pr, commit, or workflow examples",
        "a handful of links misrepresents the full release surface",
        "counts as tested only when tester-authored text appears beneath its `#### notes`",
        "what changed** and **recommended testing** never count as test evidence",
        "empty or comment-only note area means untouched",
        "one github issue comment",
    ] {
        assert!(
            normalized.contains(required),
            "release-validation contract must mention {required:?}"
        );
    }

    assert!(worksheet.contains("{{RELEASE_NOTES_URL}}"));
    assert!(!worksheet.contains("{{RELEASE_PRIORITIES}}"));
    assert!(worksheet.contains("## Upgrade findings"));
    assert!(worksheet.contains("## Priority for this release"));
    assert!(worksheet.contains("## Other subsystems"));
    assert!(worksheet.contains("> **Operator notes**"));
    assert!(worksheet.contains("<!-- Campaign creator: move 3-5 selected subsystem sections"));
    assert!(!worksheet.contains("## Private operator notes"));
    assert!(!worksheet.contains("## Release findings"));

    let subsystem_headings = worksheet
        .lines()
        .filter_map(|line| line.strip_prefix("### "))
        .collect::<Vec<_>>();
    assert_eq!(
        subsystem_headings, RELEASE_VALIDATION_SUBSYSTEMS,
        "worksheet must retain all 19 canonical subsystem skeletons"
    );
    assert_eq!(worksheet.matches("\n#### ").count(), 0);
}

#[test]
fn release_validation_preserves_state_and_private_boundaries() {
    let release_skill = read("skills/openclaw-release-validation/SKILL.md");
    let operator_skill = read("skills/ocm-operator/SKILL.md");
    let cookbook = read("skills/ocm-operator/references/command-cookbook.md");
    let safety = read("skills/ocm-operator/references/safety-and-state.md");
    let combined = format!("{release_skill}\n{operator_skill}\n{cookbook}\n{safety}");
    let normalized = normalize(&combined);

    for required in [
        "ocm adopt import --name",
        "sessions and other real user state are preserved",
        "keep the source unchanged",
        "stop the current credential owner",
        "keep ocm, copying, local tooling, setup, and cleanup problems in the conversation only",
        "never enter the worksheet or github comment",
        "remove local paths",
    ] {
        assert!(
            normalized.contains(required),
            "existing-user safety contract must mention {required:?}"
        );
    }
}

#[test]
fn operator_recipes_use_current_cli_and_safe_cleanup_contracts() {
    let usage = read("docs/USAGE.md");
    let cookbook = read("skills/ocm-operator/references/command-cookbook.md");
    let safety = read("skills/ocm-operator/references/safety-and-state.md");
    let paths = read("skills/ocm-operator/references/local-paths.md");
    let release_skill = read("skills/openclaw-release-validation/SKILL.md");
    let worksheet = read("skills/openclaw-release-validation/assets/validation-worksheet.md");
    let matrix = read("docs/OPENCLAW_RELEASE_SCENARIO_MATRIX.md");

    assert!(usage.contains("ocm logs mira --stream error"));
    assert!(!usage.contains("ocm logs mira --stderr"));
    assert!(cookbook.contains("ocm logs <env> --stream error"));
    assert!(safety.contains("git -C /path/to/worktree status --short"));
    assert!(!safety.contains("worktree remove --force"));
    assert!(paths.contains("set -euo pipefail"));
    assert!(release_skill.contains("ocm env list --json"));
    assert!(release_skill.contains("ocm runtime install --version"));
    assert!(release_skill.contains("ocm runtime verify <runtime-name> --json"));
    assert!(
        release_skill.contains("ocm upgrade <test-env> --runtime <runtime-name> --dry-run --json")
    );
    assert!(release_skill.contains("ocm start <test-env> --runtime <runtime-name> --json"));
    assert!(release_skill.contains("ocm @<test-env> -- --version"));
    assert!(matrix.contains("OCM_BIN="));
    assert!(matrix.contains("/target/debug/ocm"));
    assert!(matrix.contains("\"$OCM_BIN\" runtime build-local"));
    assert!(matrix.contains("\"$OCM_BIN\" upgrade simulate"));
    assert!(!matrix.contains("`ocm "));
    assert!(matrix.contains("run-owned package runtime is removed"));
    assert!(
        [&paths, &release_skill, &worksheet, &matrix]
            .iter()
            .all(|document| !document.contains("/Users/")),
        "release-validation docs must not publish machine-local home paths"
    );
}
