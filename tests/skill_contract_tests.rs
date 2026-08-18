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

#[test]
fn release_validation_is_manual_and_worksheet_driven() {
    let skill = read("skills/openclaw-release-validation/SKILL.md");
    let worksheet = read("skills/openclaw-release-validation/assets/validation-worksheet.md");
    let normalized = normalize(&skill);

    for required in [
        "disable-model-invocation: true",
        "one editable markdown worksheet",
        "finish validation",
        "https://docs.openclaw.ai/maturity/scorecard.md",
        "complete catalog",
        "do not use a cached or hardcoded surface list",
        "resolve relative taxonomy links",
        "rank exactly five priority surfaces",
        "change count and breadth",
        "change size and complexity",
        "maturity expectations",
        "group every user-visible or upgrade-sensitive item",
        "priority for this release",
        "other surfaces",
        "`### [surface](taxonomy-url)`",
        "| **maturity score** | <maturity-label> |",
        "| **what changed** | <release-theme> |",
        "| **recommended testing** | <exercise-or-em-dash> |",
        "`#### notes`",
        "notes` truly empty",
        "no placeholder text or hidden comment",
        "every priority surface must have a real recommended exercise",
        "bounded operator workflow",
        "exact action, the observable pass condition",
        "runnable ocm-scoped command",
        "`ocm @<test-env> -- onboard`",
        "`ocm @<test-env> -- tui`",
        "`ocm @<test-env> -- channels status --probe`",
        "no notable changes in this release.",
        "dominant themes across the surface's complete group",
        "do not include issue, pr, commit, or workflow examples",
        "a handful of links misrepresents the full release surface",
        "counts as tested only when tester-authored text appears beneath its `#### notes`",
        "table rows are campaign guidance, never test evidence",
        "empty notes section means untouched",
        "one final release-analysis comment",
        "only the surfaces with non-empty notes sections",
        "do not report the guidance table as evidence",
    ] {
        assert!(
            normalized.contains(required),
            "release-validation contract must mention {required:?}"
        );
    }

    assert!(worksheet.contains("{{RELEASE_NOTES_URL}}"));
    assert!(worksheet.contains("{{SCORECARD_URL}}"));
    assert!(worksheet.contains("{{TAXONOMY_URL}}"));
    assert!(!worksheet.contains("{{RELEASE_PRIORITIES}}"));
    assert!(worksheet.contains("## Upgrade findings"));
    assert!(worksheet.contains("## Priority for this release"));
    assert!(worksheet.contains("## Other surfaces"));
    assert!(worksheet.contains("> [!NOTE]"));
    assert!(worksheet.contains("derived from the live"));
    assert!(worksheet.contains("**Score bands:** Experimental 0–50%"));
    assert!(worksheet.contains("table-and-empty-Notes format"));
    assert!(worksheet.contains("source for the final"));
    assert!(!worksheet.contains("<!-- Add notes below. -->"));
    assert!(!worksheet.contains("## Private operator notes"));
    assert!(!worksheet.contains("## Release findings"));
    assert_eq!(worksheet.matches("\n### ").count(), 0);
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
