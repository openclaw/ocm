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
        "three to five subsystems",
        "subsystem | what changed | try",
        "group every user-visible or upgrade-sensitive item",
        "dominant themes across the subsystem's complete group",
        "optional supporting examples",
        "one or two representative links",
        "never use them as the organizing content",
        "ignore html guidance comments",
        "one github issue comment",
    ] {
        assert!(
            normalized.contains(required),
            "release-validation contract must mention {required:?}"
        );
    }

    assert!(worksheet.contains("{{RELEASE_NOTES_URL}}"));
    assert!(worksheet.contains("{{RELEASE_PRIORITIES}}"));
    assert!(worksheet.contains(
        "Themes summarize the complete release notes; linked issues are representative\nexamples."
    ));
    assert_eq!(
        worksheet
            .lines()
            .filter(|line| line.starts_with("#### "))
            .count(),
        19,
        "worksheet must retain all 19 subsystem note sections"
    );
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
        "private operator note",
        "never enters the github comment",
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
