# OpenClaw Release Scenario Matrix

This is the durable scenario list for OpenClaw release validation through OCM.
Keep it broad and update it whenever OpenClaw, OCM, installer behavior, service
behavior, plugin behavior, channels, state layout, or release packaging changes.

The goal is simple: a new Codex session should read this file, test each
scenario against both clean new-user state and copied existing-user state where
applicable, then produce a quiet report that shows what passed and exactly what
failed.

## Operating Rules

- Set `OCM_BIN=/path/to/ocm/target/debug/ocm`,
  verify it is executable, and use `"$OCM_BIN"` for every OCM command.
- Read the `origin` URL from the OpenClaw source repo at
  `/path/to/openclaw-source-cache`,
  but do not fetch into or mutate that shared checkout. Create per-run Git
  metadata from the remote, optionally using the source repo as a clone-time
  read-only object cache and dissociating the clone from it. Resolve one
  immutable `origin/main` commit and create a unique detached worktree for the
  run.
- Do not use `/path/to/active-openclaw` or `../openclaw`; that is an active
  working repo.
- In the detached worktree, run `pnpm install --frozen-lockfile`,
  `pnpm check`, and `pnpm build`, then require `git status --porcelain` to
  remain empty.
- Build a uniquely named package-shaped runtime with
  `"$OCM_BIN" runtime build-local` from the detached worktree. Use that runtime
  for the matrix, then verify the worktree is still clean.
- Use the detached worktree's `openclaw.mjs` only for the S02 direct boot smoke.
- Do not use `pnpm openclaw` as the main release-validation path.
- Use the local built OCM binary, usually `<ocm>/target/debug/ocm`.
- Use one run-specific `OCM_HOME` for the package runtime, envs, snapshots, and
  supervisor state. Invoke the explicit local OCM binary for every command.
- Use a run id in every worktree, runtime, env, report, and cleanup name.
- Copy an existing user env from `~/.ocm/envs/<existing-env>` into the run
  root before testing. Never mutate the real env.
- Treat copied and cloned state as secret-bearing. Keep services stopped and
  make no external requests by default. Use mocks, credential-free fresh envs,
  or dedicated test accounts unless the run explicitly authorizes real access.
- Use `"$OCM_BIN" env clone` only when clearing sessions, logs, and backups is
  intended. Use a copied `.openclaw` home plus `"$OCM_BIN" adopt import` for
  S20.
- Keep the report quiet: summarize pass/fail per scenario, then include details
  only for failures, blocked scenarios, or release-risk notes.
- Redact credentials, private endpoints, user identifiers, and secret-bearing
  command output from the report.
- Clean up only temp envs, services, LaunchAgents, runtimes, fixtures, and
  worktrees owned by the current run id. Destroy dependent envs before removing
  the run runtime, and inspect worktree status before removal.

## OCM Usage Primer For Test Agents

Read OCM's `README.md` and `docs/USAGE.md` before testing service, upgrade,
logs, start, clone, import, or runtime behavior. OCM is the env manager and
supervisor layer around OpenClaw; OpenClaw also has its own service/gateway
commands. Release validation must distinguish those layers instead of treating
every service side effect as an OCM bug.

Use OCM this way during validation:

- Create or bind envs with `"$OCM_BIN" start`, `"$OCM_BIN" dev`,
  `"$OCM_BIN" env clone`, `"$OCM_BIN" adopt`, and `"$OCM_BIN" migrate` as
  appropriate.
- Run OpenClaw inside an env with
  `"$OCM_BIN" @<env> -- <openclaw args>`.
- OCM env runs should inject `OPENCLAW_SERVICE_REPAIR_POLICY=external` so
  OpenClaw can report service health but does not mutate global/user-level
  service registrations from `doctor --fix`.
- Inspect env services with `"$OCM_BIN" service status <env>`,
  `"$OCM_BIN" service status`, and `"$OCM_BIN" logs <env>`.
- Simulate upgrades with
  `"$OCM_BIN" upgrade simulate <env> --to <target>`.
- Use `"$OCM_BIN" help start`, `"$OCM_BIN" help service`,
  `"$OCM_BIN" help logs`, `"$OCM_BIN" help upgrade`, and
  `"$OCM_BIN" help env` when the command shape is unclear.

Service terminology:

- OCM-managed service: background process policy/state owned by OCM for one
  environment.
- OpenClaw service/gateway: OpenClaw's own gateway/service behavior, invoked
  inside an OCM env through `"$OCM_BIN" @<env> -- ...`.
- LaunchAgent: macOS service registration. A test may intentionally create one,
  but it must match the scenario and be cleaned up.

When validating service boundaries, test user intent:

- If an env was created without OCM service management, OCM should not mark it
  service-managed unless explicitly asked.
- Do not run a broad standalone OpenClaw LaunchAgent/service mutation scenario
  unless the release itself changed service behavior.
- For normal release validation, treat service repair policy as an env execution
  assertion: OCM env runs should expose `OPENCLAW_SERVICE_REPAIR_POLICY=external`.

## Report Format

Start each report with:

- OCM commit/version
- OpenClaw commit/version
- package runtime identity
- previous release or changelog baseline reviewed
- run id
- temp paths used
- fixture modes used and whether real user state was left untouched
- external-access policy used

Then use this summary table:

| ID | Scenario | New User | Existing User | Result | Notes |
| --- | --- | --- | --- | --- | --- |
| S01 | Built artifact boots | pass | pass | pass | concise note |

Use these statuses:

- `pass`: tested and postconditions were checked
- `fail`: reproducible breakage; add a detail section
- `blocked`: not run; explain blocker and next action
- `n/a`: not applicable for that user type
- `needs coverage`: important but not exercised in this run

For failures, add a short detail block:

```text
F01 - S12 Plugin uninstall removes installed files
Impact: installed plugin is still discovered after uninstall.
Repro: <exact commands>
Expected: config, ledger, and extension files are removed.
Actual: config and ledger removed, extension directory remains.
Paste to fixer: <short actionable summary>
```

Do not paste full logs unless they are needed. Include exact commands and the
smallest useful error/output snippet.

## Scenario List

### S01 - Release Diff Review

What to test:

- Identify the last release/tag used as the baseline.
- Read changelogs and commit summaries since that baseline.
- Add temporary extra checks for changed areas not already covered below.

Pass evidence:

- Report lists baseline, commit range, changed surfaces, and extra checks added.

### S02 - Built Artifact Boots

What to test:

- Create per-run Git metadata from the source remote and resolve one immutable
  `origin/main` commit without updating the shared source checkout.
- Create a clean detached per-run worktree at that commit.
- Run the source-built executable directly with `--version` as a narrow smoke
  check.
- Build and verify a named package-shaped OCM runtime from the worktree.
- Run the package-shaped runtime through OCM-bound env execution.

Run for:

- new user: clean env bound to the built artifact
- existing user: copied env or clone bound to the built artifact

Pass evidence:

- Build and runtime verification pass, both version checks identify the
  expected commit/version, and OCM runs the package-shaped runtime.

### S03 - Release Packaging And Postinstall Shape

What to test:

- Published-package layout or local package build shape has expected entrypoints.
- Runtime files and sidecars expected by release users are present.
- Postinstall or package verification tests pass when available.

Run for:

- new user: install/start from release-shaped artifact
- existing user: upgrade simulation uses release-shaped artifact

Pass evidence:

- Package verification passes and no missing runtime entrypoints are found.

### S04 - Clean New User Start

What to test:

- Empty isolated `HOME` and OpenClaw state.
- Run-specific `OCM_HOME` contains only the verified package runtime before the
  fresh env is created; it contains no prior env, snapshot, or supervisor
  state.
- `"$OCM_BIN" start <env> --runtime <run-runtime>` creates a usable env.
- First run writes only expected minimum config/state.
- `openclaw --version`, `openclaw doctor`, `plugins list --json`, gateway
  status, and logs work.

Run for:

- new user only

Pass evidence:

- Env starts, diagnostics are clean, and no legacy or unexpected state appears.

### S05 - Existing User Upgrade From Copied State

What to test:

- Copy the source env into the run root.
- Import/adopt the copy into the run's OCM state.
- Use `"$OCM_BIN" env clone` only for scenarios that do not require sessions,
  logs, or backups.
- Keep the copied or cloned env service stopped while retained credentials are
  present.
- Bind the fixture to the package-shaped runtime.
- Run `"$OCM_BIN" upgrade simulate <env> --to <run-worktree> --scenario
  current --json`.

Run for:

- existing user only

Pass evidence:

- Simulation passes, original copied env is not mutated, the real source env
  is not touched, expected simulation steps are recorded, and no unauthorized
  external request is made.

### S06 - Upgrade Dry Run And Rollback Safety

What to test:

- `"$OCM_BIN" upgrade --dry-run` previews without mutating env metadata,
  runtime bindings, snapshots, service policy, or files.
- Failed upgrade paths roll back or clearly report retained changes when
  rollback is disabled.
- Snapshot/backup references are usable.

Run for:

- existing user: copied env clone
- new user: clean env with a prior known-good binding

Pass evidence:

- Dry run leaves no mutation; failure either rolls back or reports explicit
  retained state.

### S07 - Runtime And Launcher Binding

What to test:

- Env resolves the intended runtime or launcher.
- `"$OCM_BIN" @<env> -- <command>` uses the selected OpenClaw artifact.
- Binding changes are visible in `env show`, status, and JSON output.
- Clearing or replacing a binding does not leave stale execution paths.

Run for:

- new user
- existing user

Pass evidence:

- Effective command path is the built artifact and metadata remains coherent.

### S08 - Config Compatibility And Migration

What to test:

- Existing config files load without losing user settings.
- Legacy keys that still have migration support are migrated once.
- Removed or deprecated keys do not break startup.
- Config writes preserve unrelated user settings.

Run for:

- existing user: copied real state plus seeded legacy variants
- new user: clean config creation

Pass evidence:

- Config after command is valid, expected migrations occurred, and unrelated
  user settings remain unchanged.

### S09 - Doctor Read-Only Mode

What to test:

- `openclaw doctor` reports current state without mutating files or install
  records.
- Missing dependency checks are accurate.
- Healthy release state does not show stale errors from prior releases.

Run for:

- new user
- existing user

Pass evidence:

- No file mutation in read-only mode and diagnostics match reality.

### S10 - Doctor Repair Mode

What to test:

- `openclaw doctor --fix` repairs supported issues.
- Repair is idempotent.
- Repair does not create unrelated global state from an isolated test env.
- Follow-up read-only doctor is clean or reports only expected remaining issues.

Run for:

- new user with seeded repairable issues
- existing user with copied/legacy state

Pass evidence:

- Repair fixes targeted issues, second repair is a no-op, and no unrelated
  global state is left behind.

### S11 - Plugin Discovery And Registry Health

What to test:

- `plugins list --json` discovers bundled and installed plugins.
- Output has stable ids, enabled state, source, format, and diagnostics.
- Missing or malformed plugin residue is reported clearly.
- Healthy state has no diagnostics.

Run for:

- new user
- existing user

Pass evidence:

- Plugin list is deterministic and diagnostics are absent unless intentionally
  seeded.

### S12 - Plugin Install Compatibility

What to test:

- Valid native plugin directory with `openclaw.plugin.json`.
- Valid native plugin archive with `openclaw.plugin.json`.
- Valid Discord-style package with `openclaw.plugin.json` plus rich
  `package.json` metadata, dependencies, peer deps, compat/build fields, and
  package-level `openclaw` metadata.
- Valid Codex bundle/skill package if supported by the current release.
- Old package-json-only archive with `openclaw.extensions` but no
  `openclaw.plugin.json` fails cleanly.
- Malformed manifest, missing entry file, duplicate id, and manifest id
  different from npm package name.

Run for:

- new user
- existing user

Pass evidence:

- Valid packages install and list cleanly; invalid packages fail before writing
  config, ledger, or extension files.

### S13 - Plugin Lifecycle

What to test:

- Install, disable, enable, reinstall/upgrade, update dry-run, update all
  dry-run, uninstall dry-run, and uninstall force.
- Uninstall removes config, install ledger, and extension files.
- Path-sourced plugins are skipped or handled correctly by update commands.

Run for:

- new user
- existing user

Pass evidence:

- Final list no longer discovers uninstalled plugins and no stale diagnostics
  remain.

### S14 - Bundled Runtime Dependencies

What to test:

- Bundled plugin runtime dependencies are present in the built artifact.
- `pnpm test:build:bundled-runtime-deps` passes.
- `doctor` does not show missing bundled runtime dependency blocks on healthy
  builds.

Run for:

- new user
- existing user

Pass evidence:

- Dependency test passes and doctor output is clean for bundled deps.

### S15 - Gateway Lifecycle

What to test:

- Gateway status works before and after start.
- Explicit service start/stop/status works.
- Gateway binds to the expected env and port.
- Gateway failure leaves useful status/log evidence.

Run for:

- new user
- existing user

Pass evidence:

- Status points at the right env/binary and no stale/global gateway is mistaken
  for the env gateway.

### S16 - Logs And Support Evidence

What to test:

- `"$OCM_BIN" logs <env>` and relevant OpenClaw log commands show current env
  logs.
- JSON/raw log output is script-friendly.
- Failed startup or plugin errors are visible without digging through random
  files.

Run for:

- new user
- existing user

Pass evidence:

- Logs identify the current env and contain enough evidence to debug failures.

### S17 - Channel And Connector Smoke

What to test:

- Changed bundled channels/connectors load their manifests and config schemas.
- Channel-specific env vars and secret/config validation behave correctly.
- Telegram, Discord, Slack, MCP, browser, voice/video, TTS, or other changed
  channels do not break plugin discovery or doctor.
- Use mocks or dedicated test accounts. Do not send through credentials copied
  from a real user fixture without explicit authorization for that destination
  and account.

Run for:

- new user: clean config plus minimal channel setup when safe
- existing user: copied config with existing channel settings

Pass evidence:

- Changed channel surfaces load without diagnostics and preserve existing
  config.

### S18 - Model And Provider Configuration

What to test:

- Default model/provider changes are reflected in config and startup.
- Existing user provider settings are preserved.
- Missing provider credentials fail with clear diagnostics, not startup crashes.
- Use mocks or dedicated test credentials for live calls. Do not spend against
  copied production credentials without explicit authorization.

Run for:

- new user
- existing user

Pass evidence:

- Defaults are sane for new users and existing provider choices survive upgrade.

### S19 - Browser/UI Build And Static Assets

What to test:

- UI build passes.
- Browser automation or UI assets touched by the release are present.
- Static assets referenced by the gateway/UI resolve after build.

Run for:

- new user
- existing user through upgrade simulation

Pass evidence:

- UI build passes and no missing asset/runtime errors appear in gateway logs.

### S20 - Sessions And Persistent State

What to test:

- Copy the source env's `.openclaw` directory into the run root and import that
  copy with `"$OCM_BIN" adopt import`. Do not use `"$OCM_BIN" env clone`;
  clone intentionally removes sessions, logs, and backups.
- Before upgrading, assert that at least one expected non-empty session fixture
  exists in the imported env and record its non-secret identifier.
- Existing sessions, bindings, and conversation state can be read after upgrade.
- New sessions can be created in clean state.
- Session state paths stay under the intended OpenClaw home.

Run for:

- new user
- existing user

Pass evidence:

- The fixture exists before the upgrade, existing state remains readable after
  it, and new state is written to the env root.

### S21 - Secrets And Environment Isolation

What to test:

- Env-scoped secrets/config are resolved from the copied or clean env only.
- Parent shell `OPENCLAW_*` or OCM variables do not leak into child envs.
- Missing secrets produce actionable diagnostics.
- Copied fixtures remain offline unless credentials were replaced with mocks or
  dedicated test accounts, or real access was explicitly authorized.
- Reports and attached evidence contain no credentials, private endpoints, raw
  auth files, or secret-bearing config.

Run for:

- new user
- existing user

Pass evidence:

- Commands use the intended env root and no cross-env secret/config leakage is
  observed, no unauthorized external request is made, and report redaction is
  verified.

### S22 - Filesystem And Path Safety

What to test:

- Paths with spaces or symlinks work where supported.
- macOS `/private/tmp` normalization does not break env detection.
- Commands do not write outside temp state except intentional package caches.
- Destructive commands keep safety rails.

Run for:

- new user
- existing user

Pass evidence:

- State remains inside the intended roots and destructive commands require the
  expected confirmation/flags.

### S23 - Snapshot, Clone, Export, And Import

What to test:

- Env clone preserves usable state without sharing mutable directories.
- Snapshot/export/import round trips a test env.
- Imported env metadata points to valid local paths.

Run for:

- existing user: copied env
- new user: clean env after setup

Pass evidence:

- Round-tripped env runs the built artifact and original env is unchanged.

### S24 - Update Commands And Network Failure Behavior

What to test:

- Update commands handle available updates, no-op updates, path-sourced installs,
  and network failures.
- Network errors do not corrupt config or install records.

Run for:

- new user
- existing user

Pass evidence:

- No-op and failure paths leave state unchanged; actionable errors are shown.

### S25 - Release Notes Claims

What to test:

- Every user-facing release-note claim has at least one command-path check or is
  marked `needs coverage`.
- New features mentioned in release notes work for new users and do not break
  existing users.

Run for:

- new user
- existing user

Pass evidence:

- Report maps release-note claims to tested scenarios or explicit gaps.

### S26 - Cleanup Verification

What to test:

- Temp OCM homes, copied envs, generated fixtures, worktrees, and services are
  removed when no longer needed.
- Run-owned envs are destroyed before the run-owned package runtime is removed
  with `"$OCM_BIN" runtime remove`.
- Every cleanup target carries the current run id or another explicit ownership
  marker.
- Worktree status is inspected before removal; unclean or unowned worktrees are
  retained and reported instead of force-removed.
- The detached worktree still points at the recorded OpenClaw commit before it
  is removed.
- The real source env was not mutated.
- The source cache repo has no unintended worktree changes.

Run for:

- every validation run

Pass evidence:

- Cleanup section lists what was removed and any intentional leftovers.

## Known Risks To Keep Rechecking

- `plugins uninstall --force` must remove extension files, not only config and
  install ledger state.
- missing bundled plugin runtime dependencies must remain fixed after each pull.
- invalid package-json-only archives must fail cleanly without install residue.
- Discord-style packages with both canonical manifests and rich package metadata
  must remain installable.
