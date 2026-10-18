# Usage Guide

This guide explains how to use `ocm` in common situations and how the main command groups fit together.

If you are new to `ocm`, start with the [README](../README.md) first. It keeps the first-run path short. This guide goes deeper.

## What `ocm` manages

The main unit in `ocm` is the environment.

- `env`: one isolated OpenClaw environment
- `release`: one published OpenClaw release that you can inspect or install
- `runtime`: one local OpenClaw install managed by `ocm`
- `launcher`: one named command used to run OpenClaw
- `service`: one background OpenClaw process tied to one environment

In practice:

- use `env` every day
- use `release` and `runtime` when you want published OpenClaw versions
- use `launcher` when you want a local checkout or custom command
- use `service` when an environment should keep running in the background

## The quickest ways to get started

### Guided setup

Use this if you want `ocm` to walk you through the choices:

```bash
ocm setup
```

This is the easiest path for:

- first-time use
- choosing between stable, beta, exact version, or local checkout
- letting `ocm` suggest an environment name

### Fast start

Use this if you already know what you want:

```bash
ocm start
```

Or with a specific choice:

```bash
ocm start mira --channel beta
ocm start mira --version 2026.3.24
ocm start luna --command 'pnpm openclaw' --cwd /path/to/openclaw
```

`start` creates or reuses the environment, prepares the chosen OpenClaw source, and can run onboarding for you.
By default, it also installs and starts the environment service so OpenClaw keeps running in the background.

## Common scenarios

### 1. Use the latest stable release

```bash
ocm start
```

Or:

```bash
ocm start mira --channel stable
```

Use this when you want the normal supported path without worrying about local checkout details. `start` keeps the environment running in the background unless you pass `--no-service`.

### 2. Try the beta release

```bash
ocm start mira --channel beta
```

Use this when you want a published prerelease without moving to a local development build.

### 3. Pin to an exact published release

```bash
ocm start mira --version 2026.3.24
```

Use this when you need repeatability across machines or teams.

### 4. Run a local checkout

```bash
ocm start luna --command 'pnpm openclaw' --cwd /path/to/openclaw --no-service
```

Use this when you are developing OpenClaw locally or want a custom run command.

Use `ocm dev luna --repo /path/to/openclaw` to run that exact checkout with separate
environment state. New dev environments borrow main or registered linked
checkouts without creating a Git worktree. Outside a checkout, pass `--repo`;
otherwise OCM can use the checkout enclosing the current directory. A resumed
borrower keeps its canonical source path and refuses a different explicit or
enclosing checkout. Existing OCM-owned environments retain their recorded
worktrees and original repository selection.

New borrowers may prepare missing tooling with `pnpm install --frozen-lockfile`.
Resumed borrowers report missing tooling for explicit preparation; they do not
reinstall dependencies. Removing a borrower preserves source files, dependencies,
generated output, and unrelated source processes. Older OCM readers refuse the
new binding records. Refresh an incompatible running daemon with
`ocm service refresh-daemon --acknowledge-gateway-restarts` before registration.
If an environment containing or owning the source or its required Git metadata is
busy, retry dev creation or local upgrade simulation after its operation finishes.

Foreground dev starts the native Gateway watcher and live UI by default.
`--no-ui` runs the Gateway watcher alone; `--no-watch` keeps the live UI with a
Gateway that does not rebuild automatically. Combine both for a plain foreground
Gateway. `--watch` and `--ui` remain explicit aliases for the defaults; each
conflicts with its negative counterpart.

New dev environments publish a private config with a persistent Gateway token
before registration, so paired clients can reconnect after a restart or source
rebuild. Initialization preserves existing config files, including authored auth
and SecretRefs. Repeating minimum setup preserves unchanged config bytes.

OCM owns the native Vite process and prints its initial
native browser handoff once both documents are ready. The UI uses a captured
loopback address retained across stop/start for that environment. A busy retained
port is an error; OCM does not silently choose another address. Clone and import
select new addresses, while snapshot restore keeps the current environment's
reservation. Removing the environment releases it. `ocm dev status luna` reports
the address. On Linux, macOS, and Windows, a matching repeated start requests a
fresh native browser grant from the existing controller without restarting
Gateway or Vite. Busy or unready requests report a pending link.
Templated `gateway.controlUi.basePath` values are resolved by the selected
checkout's native config command before startup, including OpenClaw's `.env`
and config environment rules. The private read preserves the authored config,
and repeated starts keep the original controller's resolved target. If variables
remain unresolved, supply them or use a concrete base path before retrying;
`--no-ui` still starts the Gateway without a UI target.
Older OCM controllers retain address-only reuse and report fresh links as
unavailable until that dev session is restarted.
`ocm dev stop luna` stops the components together. A pending initial request gets
30 seconds and retains its helper until completion. Repeated requests have the
same deadline; late or disconnected callers' grant bytes are discarded.
An unfinished or interrupted cleanup keeps its recorded ownership.
UI requires HTTP, enabled Control UI and installed source UI dependencies; use
`--no-ui` for TLS or disabled-UI environments. `--service` uses the background
workflow without foreground watching or UI and rejects explicit `--watch` or
`--ui`. Existing authentication settings remain in effect.

If you run `ocm setup` from inside an OpenClaw checkout, local mode can detect that and fill in sensible defaults.

### 5. Test a local checkout as a release-shaped runtime

```bash
ocm runtime build-local main-local --repo /path/to/openclaw --force
ocm runtime build-local main-with-codex --repo /path/to/openclaw --companion codex --force
ocm start luna --runtime main-local
ocm upgrade luna --runtime main-local
```

Use this when you need to test what users will run after an OpenClaw release, but from a local checkout before publishing. OCM runs `npm pack` in the OpenClaw repo, which triggers OpenClaw's package prepack path instead of registering a source checkout command.

When the root package depends on private `workspace:*` packages, OCM resolves
their complete transitive closure. Its build-only npm proxy rewrites nested
workspace specs to exact local versions only inside scratch archives, leaving
the selected checkout unchanged.

Repeat `--companion <plugin-id>` when the checkout contains an official plugin that is version-bound to the selected OpenClaw build but published separately. OCM uses OpenClaw's package-local plugin release contract, requires the plugin package version and `openclaw.build.openclawVersion` to match the root package exactly, installs its dependencies in an isolated runtime-owned tree, and stages it under `dist/extensions/<plugin-id>` so OpenClaw discovers it as bundled. The runtime record stores the package version plus artifact and entrypoint hashes, and `runtime verify` checks the installed entrypoint hash. OCM refuses missing, mismatched, non-publishable, duplicate, or already-bundled companion selections before publishing the runtime.

The installed runtime uses the same package layout as published OpenClaw runtimes:

```text
files/node_modules/openclaw/openclaw.mjs
files/node_modules/openclaw/dist/extensions/codex/
```

That matters for release and upgrade testing because it avoids source/Jiti execution paths and exercises the built package files that users install.

When an environment should run an immutable build of a local checkout while
retaining extensions that are available to the source launcher but intentionally
omitted from the release-shaped core package, opt in explicitly:

```bash
ocm runtime build-local upstream-main \
  --repo /path/to/openclaw \
  --include-source-extensions \
  --force
```

OCM packages only built extensions under that checkout's `dist/extensions`
that are absent from the core tarball, installs their runtime dependencies, and
places them in the managed runtime's bundled-extension root. Extensions from
global, state, workspace, or other external paths are not included or promoted.
Without the flag, `build-local` remains release-shaped.
Because the opt-in installs every omitted extension and its dependency closure,
the resulting runtime can take longer to build and use substantially more disk
space than the release-shaped default.

To build for an existing environment without including unrelated source
plugins, name the target explicitly:

```bash
ocm runtime build-local primary-test \
  --repo /path/to/openclaw \
  --for-env primary \
  --force
```

OCM reads the target environment's effective config before `npm pack`, validates
explicit plugin references against the checkout and installed-plugin records,
and follows transitive local plugin package dependencies. The resulting runtime
adds only required source plugins omitted from the core package. `--for-env` and
`--include-source-extensions` are mutually exclusive. A build without either
option remains release-shaped.

### 6. Keep an environment running in the background

```bash
ocm start mira
ocm service status mira
```

`start` and `setup` already do this by default. Use `service install` directly when you skipped the background service earlier with `--no-service`.

### 7. Update OpenClaw later

```bash
ocm upgrade mira
ocm upgrade --all
ocm upgrade mira --dry-run
ocm upgrade mira --runtime main-local
ocm upgrade simulate mira --to 2026.4.20
ocm upgrade simulate mira --to 2026.4.20 --scenario all
ocm upgrade simulate mira --to beta --scenario all
ocm upgrade simulate mira --to ./openclaw
```

Use this when a newer OpenClaw release is available and you want your environments to move forward without manually updating runtimes and restarting services.
Use `upgrade simulate` when you want to test what would happen against a published release or local OpenClaw repo before touching the real env.

`upgrade` is env-first:

- channel-tracked runtimes move forward
- simulations clone the source env, run OpenClaw's update dry-run plan for published targets, validate local repo builds for repo targets, then run update-mode doctor, plugin update dry-run, and gateway status checks
- simulation envs and temporary runtimes are cleaned up automatically; use `--keep-simulations` only when you need retained debug artifacts
- missing published targets fail before any simulation env is created
- `--scenario all` runs built-in current, clean minimum, and Telegram-configured env shapes as separate simulation clones
- target runtime installation, checkpoint preflight, and runtime recovery preparation run while the current managed gateway remains available; preparation failure leaves it unchanged
- a running managed gateway is stopped only for checkpoint capture, runtime publication, environment mutation, and finalization
- new checkpoints capture the complete environment root; APFS clone support is used when available and a metadata-preserving full copy is the fallback
- when an env moves to a new runtime, OCM runs OpenClaw's update finalization path cold before service restart
- if service reconciliation fails, OCM restores the snapshot and previous runtime unless `--no-rollback` is set
- pinned runtimes stay pinned unless you pass `--version`, `--channel`, or `--runtime`
- local-command environments are reported clearly instead of being changed behind your back

### 8. Run OpenClaw without activating the shell first

```bash
ocm @mira -- status
ocm @mira -- tui
ocm @mira -- onboard
```

Use this for quick one-off runs.

### 9. Activate an environment in your current shell

```bash
eval "$(ocm env use mira)"
ocm -- status
```

Use this when you want to stay inside one environment for a longer interactive session.

## Choosing between releases, runtimes, and launchers

This is the part that usually needs the most explanation.

### Use `release` when you want to browse what is published

Examples:

```bash
ocm release list
ocm release list --channel stable
ocm release show --channel stable
ocm release show 2026.3.24
```

`release` does not run OpenClaw by itself. It tells you what is available to install.

### Use `runtime` when you want a local installed OpenClaw

Examples:

```bash
ocm release install --channel stable
ocm runtime build-local main-local --repo /path/to/openclaw --force
ocm runtime build-local main-with-codex --repo /path/to/openclaw --companion codex --force
ocm runtime list
ocm runtime show stable
ocm runtime verify stable
```

Use a runtime when you want:

- a published OpenClaw release installed locally
- a local OpenClaw checkout built and installed in the same package shape as a release
- stable naming like `stable` or `2026.3.24`
- verification and updates

Runtime release JSON, including official registry metadata and custom manifests,
is limited to 64 MiB after gzip decoding. Custom sources larger than this limit
now fail with a size-limit error. Rejection happens before runtime replacement
and preserves the installed runtime's files and metadata; reduce the manifest
size before retrying an update.

Direct runtime replacement, update, local rebuild, and removal refuse when an environment uses that runtime or a registered alias to its managed files.
The environment can be stopped: its persistent binding still protects the executable.
Per-environment upgrades and rollbacks apply the same dependency check to sibling environments before changing runtime bytes.
Use an isolated release for an independent upgrade, or clear dependent bindings before intentionally replacing an unused runtime.
An idle unbound alias does not prevent mutation, and unrelated runtimes remain usable even when another registered executable is missing.

Runtime registrations and managed directories share a namespace: the registration for `old` occupies the install directory of `old.json`.
OCM refuses an installation that would replace another runtime's registration or payload, including with `--force`; choose another name when these paths conflict.
Names ending in `.json` remain supported when their destinations are available, and `--force` still replaces an unbound runtime of the same name.

### Package runtime Node.js selection

Official package installation reads `engines.node` from the integrity-verified archive before invoking npm.
Ordinary runtime execution reads the installed package manifest and uses a compatible host Node.js when available, otherwise a compatible private toolchain.
OCM validates cached executables with `node --version`; older caches remain usable offline for compatible packages.
If neither the host nor OCM's approved private Node.js version satisfies the package, install a compatible host Node.js and npm before retrying.
Packages without a declared Node.js engine retain the legacy baseline; source launchers and manually registered commands retain their selected toolchains.
`doctor host` and its `officialReleaseReady` field describe baseline availability and managed-toolchain support, without resolving any particular package version.
A ready host check therefore does not guarantee that every selected release is compatible.

### Use `launcher` when you want a command recipe

Examples:

```bash
ocm launcher add dev --command 'pnpm openclaw' --cwd /path/to/openclaw
ocm launcher list
ocm launcher show dev
```

Use a launcher when you want:

- a local checkout
- a wrapper script
- a custom command line
- something that is not just a published OpenClaw release

On Unix, recipes beginning with the `exec` or `eval` shell builtin use the shell for managed services as well as `env run`, with or without quotes in the recipe. In particular, `eval` retains its shell evaluation semantics instead of being treated as an external executable.
Plain executable recipes still run directly under the supervisor.

### The simple rule

- published OpenClaw release: use `release` and `runtime`
- local checkout as source command: use `launcher`
- local checkout as built package: use `runtime build-local`; add
  `--include-source-extensions` only when the managed artifact should retain
  source-checkout extensions omitted from the release package
- day-to-day work: use `env`

## Running commands inside environments

### Activate the environment

```bash
eval "$(ocm env use mira)"
```

After that:

```bash
ocm -- status
ocm -- onboard
```

This is the shortest interactive path.

### Run without activating

```bash
ocm @mira -- status
ocm @mira -- tui
ocm @mira -- onboard
```

This is the shortest non-interactive path.

### Run any other command in the environment

```bash
ocm env exec mira -- sh -lc 'echo "$OPENCLAW_HOME"'
```

OCM env execution sets OpenClaw's env-scoped paths for you:
`OPENCLAW_HOME`, `OPENCLAW_STATE_DIR`, and `OPENCLAW_CONFIG_PATH`.
It also sets `OPENCLAW_SERVICE_REPAIR_POLICY=external`, which tells OpenClaw
that service registration and repair are managed outside OpenClaw in this
context. Commands like `openclaw doctor --fix` can still repair normal env
state, but background service lifecycle should be handled with `ocm service`.

### Export one bounded artifact

On Unix, export a file relative to an environment's `OPENCLAW_HOME` without
executing an environment command:

```bash
ocm env artifact export mira \
  --path .openclaw/openclaw.json --max-bytes 1048576 > config.json
```

`--path` and the nonnegative decimal `--max-bytes` are required. stdout contains
only the raw file bytes; errors go to stderr. There is no destination option.
The caller owns the destination and must discard it on a nonzero exit, including
any partial bytes. Consumers must independently limit incoming bytes and time.

The registered environment home must be a directory, not a symlink. Beneath
that home, OCM opens each component relative to pinned directory descriptors
and rejects traversal, symlinks, nonregular files and files with multiple hard
links. It checks the file's size, identity, link count and modification/change
timestamps before and after reading, and rejects observed changes or leaf
replacement. This detects instability, but is not an atomic snapshot or an
attestation of candidate-produced data. Stop the producer before exporting
final diagnostics.

The command uses the invoking OCM user's permissions and does not change service
or environment state. When crossing user boundaries, run the whole OCM command
as the environment owner; the receiving process retains destination ownership.
Non-Unix platforms fail closed because this command requires descriptor-relative
no-follow access. Existing archive export and other commands are unchanged.

## Service management

Use `service` when an environment should run in the background.

### Install and inspect

```bash
ocm service install mira
ocm service list
ocm service status mira
```

Use `service install` when you want a background service for an environment that was created with `--no-service`, or when you want to bring an older env under background management later.

On Unix, OCM waits for Gateway PID publication before moving a matching caller
out of the Gateway process group during daemon refresh, snapshot, or upgrade
maintenance. This protects the caller from the supervisor's signals to that
group. A systemd unit stop or restart can still terminate the caller through its
service cgroup.

### Read logs

```bash
ocm logs mira --tail 50
ocm logs mira --stream error
ocm logs mira --follow
```

### Start, stop, restart

```bash
ocm service start mira
ocm service stop mira
ocm service restart mira
```

`service stop` keeps the env installed with `desiredRunning: false`. When no env
is desired running, OCM removes the idle shared daemon. Status then reports the
daemon as absent without treating that absence as a fault. Leave it absent while
the envs should stay stopped; `ocm service start <env>` recreates it when needed.
`refresh-daemon` requires an existing daemon and is not an installation command.

If the daemon reports `cleanup is unverified`, it retains the affected child in its runtime ownership record and does not start a replacement for that child during the current daemon run.
A failed signal, process inspection, or bounded wait is not evidence that the old process group stopped.
Other environments can continue to run or stop.
Resolve the reported operating-system error before requesting another restart; do not delete the runtime record or signal a saved PID without verifying its current identity.
The first explicit `service restart <env>` (including `--force`) retries cleanup even after Stop saved stopped intent. It succeeds only after observing a replacement; if cleanup still cannot finish, Restart reports an error and retains the cleanup evidence for another retry.
A daemon shutdown with unresolved cleanup returns an error.

Starting a service requires a successful manager observation or confirmed absence of the OCM daemon.
If the manager cannot be queried or reports an unknown process state, OCM refuses activation and preserves the existing daemon and other environment services.
Retry after restoring manager access; `ocm service status` remains available for best-effort inspection.

Saved dev service plans must still match the env's current binding and recorded
worktree. If OCM refuses a stale plan, use `ocm service restart <env>` to refresh it.

Normal restart is gateway-aware when `ocm service status mira` reports restart
handoff `protocol v1`: OpenClaw records eligible active sessions and subagents,
hands the fresh-process restart back to OCM immediately, and resumes recoverable
work after the replacement gateway starts. The old gateway process does not wait
for an in-flight turn to finish.

External restart requests collect the native helper result while observing replacement for up to 30 seconds; a rejection fails the command even if the original Gateway remains healthy.
A pending helper or unobserved replacement produces a warning without forcing a supervisor restart.
Requests made inside the target Gateway remain asynchronous so the caller cannot block its own restart.

When a binding cannot negotiate the restart handoff, OCM preserves the existing
direct-supervisor restart behavior and prints a warning that in-flight work may
have been interrupted. Existing restart commands therefore remain compatible.

Use the forced path only when a gateway advertises recovery support but is too
unhealthy to accept the restart handoff:

```bash
ocm service restart mira --force
```

Forced restart explicitly replaces the supervised child directly. It bypasses
OpenClaw's restart-recovery handoff and can lose in-flight work, so it is
intentionally an emergency override rather than a compatibility requirement.

If a legacy or forced restart is not observed within 30 seconds, OCM reissues only that environment's restart request.
Recovery activates the shared daemon only after confirming it is absent or stopped, retaining unrelated child definitions and restart requests.
A failed or unknown service-manager observation returns an error without replacing the daemon or interrupting healthy siblings.
Restore service-manager access or wait for its state to settle, then retry.
The target request remains pending, so the existing daemon can still process it.

A Gateway that exits cleanly without an accepted restart handoff stays stopped
while its saved launch definition remains unchanged. Starting or restarting a
sibling environment does not revive it. An explicit restart of that environment,
a changed launch definition, or a new daemon run can start it again. Stopping the
service removes its inactive runtime record so snapshot maintenance can verify
that the environment is quiescent.

Snapshot and upgrade maintenance check the target's supervisor ownership before capturing or replacing its state.
A failed service-manager read, a running daemon without a current runtime observation, or a runtime identity mismatch fails maintenance before mutation; stored stopped intent alone is insufficient.
If this check fails while pausing a running target, OCM restores its prior service intent without restarting the shared daemon.
Resolve the reported observation problem and retry; for a daemon without process identity, refresh it from the current installation during an authorized maintenance window.
These checks cover the native service observation and the supervisor's planned and recorded target ownership.
They cannot discover older workloads that were never recorded or processes outside the supervisor's owned cleanup scope; refreshing the daemon does not prove those processes stopped.

Restart-handoff capability and consume helpers have a 15-second completion budget that includes stdout and stderr collection, followed by bounded cleanup.
If a descendant keeps either output open, OCM stops the helper's owned process group on Unix and closes its readers before continuing sibling supervision.
A capability timeout permits legacy startup only after cleanup is verified; otherwise the target stays stopped with a cleanup error and is not automatically retried for the unchanged launch definition.
Resolve the reported helper or descendant problem before explicitly restarting that target.
A failed or incomplete consume operation never authorizes a replacement Gateway.

Recovery is limited to work OpenClaw knows how to persist and resume. It does
not make arbitrary child processes or non-idempotent external side effects
transactional.

### Remove the service

```bash
ocm service uninstall mira
```

This removes the background service. It does not remove the environment.

## Upgrading OpenClaw and `ocm`

### Upgrade one environment

```bash
ocm upgrade mira
```

This is the normal command when `mira` tracks a channel like `stable` or `beta`.
Use `--dry-run` to preview the transaction without writing snapshots, runtimes, envs, or services.
This is a plan, not a readiness check: candidate preparation, configuration
migration, and candidate validation do not run. The text output and JSON `note`
identify these limits, including for batch previews. A `would-switch` or
`would-update` outcome can still be followed by a preparation failure during the
real upgrade. Use `ocm upgrade simulate mira --to <version|channel|local-repo>`
for deeper checks against an isolated copy; simulation does not guarantee a
successful live upgrade.

In `--json` output, `runtimeReleaseVersion` and `runtimeReleaseChannel` describe
the resolved target even when preparation fails or the upgrade rolls back.
The version includes a named runtime's detected version; the channel remains
`null` when the selected runtime's channel is unknown.
Final verification compares the running command with the named runtime version detected at admission, falling back to release metadata when no comparable version was detected.
A mismatch fails the upgrade.
When automatic rollback is enabled, recovery is reported as restored only after the previous binding passes verification.

A named target remains protected from OCM runtime replacement or removal from resolution through upgrade completion or recovery.
Existing shared readers and unrelated runtime mutations can continue.
On Unix, native upgrade commands retain this protection if they outlive the OCM parent, until those commands exit.

On Linux and macOS, a no-target upgrade can update a recognized `pnpm openclaw` or `node <checkout>/openclaw.mjs` checkout in place while keeping its launcher binding.
On Unix, literal single- or double-quoted paths can contain spaces, including aliases to a source checkout.
Quoted characters such as `#` and `&` are recognized as path data, and single quotes also preserve literal `$` and backtick characters.
Backslashes outside single quotes, active substitutions, and unquoted shell operators remain opaque.
Recognizing literal words for inspection does not change how launcher commands execute.
This requires native source artifact observations from `openclaw update status --json`; older checkouts without that observation remain `local-command`, with support reported as unknown.
OCM refuses dirty or unverified checkout status, known shared users, and relevant active or unresolved foreground source ownership before source mutation.
An unfinished session in another environment does not block the update when its recorded source is provably independent of the checkout and its Git metadata.
The target's own unfinished ownership still blocks updates, and temporary takeovers with unknown or mismatched source generations remain protected.
Source execution also refuses assume-unchanged or skip-worktree index entries, which can hide modified tracked files from ordinary Git status.
Native update activity or unresolved run status must also be settled before OCM captures a checkpoint or changes the service.
A no-op requires a current target and verified native artifacts; a running service also requires matching Gateway readiness, build identity, and applied launch settings.
This leaves configuration and service untouched.
Development source requires a freshly checked `main` upstream; configured stable or beta source requires the native `preferredTarget` observation, including its channel and commit.
Missing target observations remain unknown and cannot establish this shortcut.
Otherwise, OCM captures an environment checkpoint, stops its managed service, and invokes the public native `update --no-restart` command with external service repair policy.
Native OpenClaw owns source selection, staging, builds, Doctor, and source recovery; OCM restores its prior service policy only after verification.
Post-update and recovery verification prefer fresh local artifact observations from ordinary OpenClaw status.
When that command cannot supply complete observations, such as for an unconfigured environment, verification uses native update status and can still wait for remote discovery.
An observed wrong installation or unready build remains a verification failure.
Binding changes, service-policy writes, and new foreground source admission wait while the native source operation holds registry exclusion; environment and job-status reads remain available.
Source preparation also refuses while an overlapping source environment is completing an operation, including service activation and recovery.
Successful source execution records `source-updated`, which cannot be selected by `upgrade rollback`, including in older OCM readers.
If recording history fails after source and Gateway verification, the operation reports `failed` with a history-incomplete note and retains the checkpoint; the verified source and service remain in place.
An interrupt before completion still follows the existing source recovery policy.
The checkpoint contains environment state, not the previous source bytes, so OCM never replays it over an unverified source runtime.
When native recovery cannot prove compatible software and state, the report retains the checkpoint and identifies unresolved recovery or service shutdown instead of claiming restoration.
Inspect the native recovery report before restarting through OCM.
After forcibly terminating the native updater, inspect its run and process state before changing source bindings or restarting the checkout.
Source execution rejects `--no-rollback` because that flag cannot disable native recovery; explicit runtime, version, and channel conversions retain their existing behavior.
Job capabilities describe the recognized source command route without executing OpenClaw or testing current eligibility, so status remains readable throughout an update and recovery.
Source `--dry-run` remains local inspection without executing OpenClaw or probing remote update support.
JSON includes `source` only for recognized source launchers, with the canonical `root`, Git `head`, `builtCommit` and `builtVersion` from `dist/build-info.json`, and nullable `buildMatchesHead` and `workingTreeClean` observations.
A different built commit reveals source that has moved without a matching build; a matching commit alone does not prove a complete build, healthy dependencies, or the identity of a running Gateway.
Missing or invalid metadata remains unknown and is explained in `issues`.
Working-tree cleanliness also remains unknown when Git filters or submodules prevent inspection without potentially executing configured commands.
Staged submodule removals and replacements remain visible as dirty when no submodule remains in the index; inspection does not scan nested worktrees.
Working-tree cleanliness remains unknown for repositories with partial-clone or promisor-remote configuration, because Git can fetch missing objects while checking status, including on older versions that ignore lazy-fetch safeguards.
Locally recorded HEAD and tracking information remain available without loading those objects.
`trackingRef` and `trackingHead` describe locally recorded Git tracking information, which may be stale; inspection does not fetch or contact a remote.
`sharedEnvironments` lists other registered launcher, dev, or runtime bindings whose known paths overlap the checkout, including path aliases.
An empty list does not establish exclusive ownership or exclude unmanaged processes.
Opaque shell launchers retain their existing report without inferred source facts.

An unavailable optional candidate Doctor check is a compatibility exception only when it is the sole failure, including when native Doctor repeats that same finding on stderr during an update.
This preserves upgrades when the selected plugin check is disabled or absent from an older runtime.
Mixed failures, malformed responses, and additional or contradictory diagnostics stop the upgrade before finalization and binding publication.

When candidate validation fails, the report keeps bounded, redacted Doctor
findings and identifies candidate-provided hints as information captured before
recovery. OCM's recovery guidance describes the current environment binding and
whether the failed candidate was retained, removed, or replaced by the previous
runtime. With `--no-rollback`, completed target configuration repairs remain in
the environment. Per-environment upgrade history records only cleanup diagnostics,
excluding general child output. Fleet batch journals retain the same bounded,
redacted result notes shown by the batch command.

Automatic rollback verifies that the restored binding executes and, when the original version is known, reports that version before accepting recovery.
Previously running services must also pass Gateway verification; stopped services remain stopped.
A failed verification retains the displaced environment state and reports its restore-operation location.

If automatic rollback or recovery from a failed explicit rollback cannot finish, OCM preserves the previous managed runtime files along with the existing safety snapshot.
The result remains `rollback-failed` and prints the retained file location; it does not certify restored configuration, runtime, or service health.
Inspect `ocm upgrade history <env> --json` for the transaction's snapshot and runtime recovery metadata.
If writing recovery metadata also failed, preserve the original backup location printed in the command result.
Keep the snapshot and retained files until operator recovery is complete, because snapshot removal also removes linked runtime recovery.
Automatic recovery after abrupt process loss is not provided by this retention behavior.

On Linux and macOS, upgrade commands keep the environment mutation lock in their native version probes, Doctor, finalizer, completion-cache, and final Gateway-status verification children.
Version probes retain the lock during upgrade preflight, final verification, and rollback because configured Node preloads and launcher arguments can run before the version fast path.
The same protection applies to final Gateway-status verification after an explicit rollback, because native diagnostic startup and exit can write environment state even for status queries.
If the OCM parent is killed, competing environment mutations and upgrades wait until those children close the inherited lock or exit.
Each fleet child retains only its own environment's lock.
This is not crash recovery or proof that all descendants stopped: a program can close inherited descriptors before spawning further work, and Windows retains its existing locking behavior.

If a later mutation keeps waiting, use `lsof "${OCM_HOME:-$HOME/.ocm}/locks/environments/mira.lock"` to inspect processes with that environment's lock file open.
Distinguish waiting OCM commands from surviving upgrade children, and let active update work finish.
Stop an unintended holder only after verifying that interrupting its work is safe, then retry the mutation.
Do not delete or replace the lock file to bypass a holder: a new file can admit another writer while the original holder is still running.

### Upgrade every environment that can be updated safely

```bash
ocm upgrade --all
```

This updates channel-tracked environments and restarts their running services when needed.

### Move a pinned or local env to a different published release

```bash
ocm upgrade mira --channel beta
ocm upgrade mira --version 2026.3.24
ocm upgrade mira --runtime main-local
```

Use this when you want to deliberately move one environment to a different published release or to an already installed runtime, such as a release-shaped local build created with `ocm runtime build-local`.

### Update `ocm` itself

```bash
ocm self update
ocm self update --check
```

## Environment lifecycle

`env create`, `start`, and `dev` accept explicit Gateway ports from 1 through 65535 with `--port`.
An empty, zero, or out-of-range value is rejected before creating an environment or changing its configuration.

New environment roots must be separate. A root cannot equal, contain, or sit
inside another registered environment root, including through path aliases.
OCM rejects overlap before creating or copying environment state or registering
the new environment, regardless of protection flags. Default sibling roots and
disjoint custom roots remain valid. This applies to `env create`, `env clone`,
`env import`, and new environments created by `start`, `setup`, `dev`, `migrate`,
`adopt import`, or upgrade simulation.

A registered root remains reserved while its directory is missing. Disjoint
Unicode roots remain valid; case and normalization aliases of a missing root
are still reserved. On Windows, an ambiguous missing 8.3 short name requires
restoring the registered path before retrying.

These operations also require a root outside registered dev sources. Missing borrowed
source paths remain reserved until their binding is removed; missing paths of
OCM-owned worktrees can still be reused. The root must neither contain
a registered source nor be inside it. OCM resolves source and destination aliases and also protects source
symlinks that failed clone or import cleanup would remove. Choose a separate
environment root.

When OCM reports that it cannot resolve or inspect a registered source, it stops
before writing the destination. Check the recorded source with `ocm env show
<env>`, restore that checkout's path or access permissions, then retry.

Removal, pruning, destroy, and simulation cleanup also protect checkouts and Git
metadata used by another environment's active temporary source session, including
`ocm dev <env> --repo <checkout> --watch --force`. A containing cleanup refuses
before changing state, even with `--force`; run `ocm dev stop <env>` for the
identified temporary owner first. Disjoint sources do not block cleanup, and a
completed temporary session does not reserve its old checkout. If an unfinished
session's source cannot be established, finish or recover that session before
retrying cleanup.

### Clone an environment

```bash
ocm env clone mira rowan
```

Automatic Gateway port selection searches upward from the preferred port through 65535, preserving existing environment and dev UI reservations. If no candidate remains, creation, cloning, or archive import fails without registering an environment or creating its destination root; it does not wrap to a lower port.

Clone copies the workspace and env config into a new environment, gives the clone its own gateway port, rewrites env-scoped OpenClaw config paths under the new env root, keeps durable agent auth/settings for the same user, clears copied runtime residue like sessions, logs, and backups, and keeps the background service separate. Configured agent directories inside the environment retain their payloads through clone and ordinary export/import, including custom and nested locations.
Agent directories remain separate from workspaces; external locations and paths reached through symlinks are rejected by these copy operations.
Upgrade simulation rejects an identity-dependent agent directory supplied by an include shared with unrelated configuration fields; give those consumers separate include files before retrying.
Git repositories whose administrative directories are already contained in retained workspaces rebase owned core.worktree and include paths to the destination.
Copied configuration referenced by these repositories survives cleanup without retaining unrelated siblings or acquiring external files.
Git continues to select conditional includes, and authored predicates, repeated-entry order, comments and unrelated signing/remote settings remain intact.
Relative values keep Git's containing-file or Git-directory context; external references and inactive missing or invalid includes remain unchanged unless their path maps to copied content.
Config files retain their copied permissions; unsafe ownership, active invalid configuration, required writes into protected history, or configuration locks fail the clone before registration and roll back its target.
Linked-worktree registrations and other Git storage pointers retain their separate copy handling.

Clone does not copy dev source bindings. The usual next step is:

```bash
ocm start rowan
```

Clone (including upgrade simulation), `migrate`, and `adopt import` use SQLite snapshots for recognized operational databases: shared state and quarantine, canonical agent stores, legacy `memory/*.sqlite` stores, and the default Workboard, Team Reports, Logbook, and active Matrix account state stores.
Agent database paths recorded in the source shared-state registry are also included when the existing copy traverses them.
Each database is a coherent committed snapshot; databases and other files are not captured in one atomic transaction.
The source stays running and its database contents are not rewritten.

Plain-home migration (`migrate` and `adopt import`) rebases retained SQLite session locations and database registry paths into the copied state directory and configured external workspace copies, including filesystem aliases.
Session locations follow the actual shared workspace projection, while in-home database registry paths stay in the fixed copied state directory.
When a configured workspace contains the original home, its workspace locations select the enclosing workspace copy; active runtime databases still use the new `.openclaw` state directory.
Recognized legacy session headers with an in-home `cwd` follow the same workspace projection; other legacy text and external references keep their existing relocation policy.
It maps resolved locations so internal links do not leave retained sessions pointing back at the source; references outside the copied roots and unresolved links remain unchanged.
It uses the copy operation's recorded database destinations, including registered databases already copied from external workspaces, without copying additional external contents.
Authored database payloads, approval and integrity metadata, and protected history remain unchanged; the source databases are never rewritten.
The original home's history protection also applies when those files are carried into an enclosing workspace copy.
Retained managed Git worktree bindings stay consistent with the copied repositories and checkouts, preserving session/worktree IDs and checkout content.
Required copied Git configuration survives cleanup, and registry paths, structural Git links and selected configuration must agree before migration succeeds.
A required database or Git relocation failure rolls back the new environment without acquiring additional external Git data or changing source registrations.
Clone and upgrade simulation also rebase retained SQLite session paths, including `sessionRoot` and `spawnedCwd`, from the source environment root to the copied root.
This includes `openclaw-agent.sqlite` in configured agent directories, even when an ordinary archive import omitted the global database registry.
This includes legacy live session entries that OpenClaw migrates into its current session store.
External paths and integrity-protected history keep their existing values, while clone continues to clear copied runtime residue and database leases.
Retained managed Git worktrees whose repository and required Git data are inside the copied environment keep their checkout content, including the default `.openclaw/worktrees` layout.
Clone relocates their registry identity and structural Git links together, preserving session/worktree IDs, branch and snapshot refs, indexes, and authored content.
It retains and relocates owned Git storage links needed by those checkouts.
It fails and removes the incomplete target when required Git data is missing or external, copied locks/run ownership remain unresolved, or a required Git write would change integrity-protected history; resolve the reported condition before retrying.
Clone does not copy external repositories, clear Git locks, or change source registrations.
Clone refuses an active database that resolves to protected history, including through directory aliases, and removes the partial clone before registration.
The same active-state ownership check applies to migration and its existing import rollback.

OCM does not classify a package or project asset as operational merely because it has a SQLite header or filename.
Other unregistered custom database locations and opaque package, project, import, audit, and Workshop payloads retain their existing copy behavior.
Symlinks keep their existing handling.
A selected database that cannot be snapshotted makes the command fail and invokes its existing target cleanup; raw source WAL or journal files are never attached to its snapshot.

Registered managed plugins use the clone's installed files. Local project and
archive paths can remain as source information once the installed payload's
location is proved to belong to the clone. Missing managed payloads stay missing,
with their install records rebased onto the target for diagnosis or repair.
Active plugin paths and copied databases must remain inside the cloned environment;
locations that escape through external paths or symlinks are rejected before publication. The
same plugin isolation applies to upgrade simulation clones.

Clone (including upgrade simulation) and import require real directories at `.openclaw`, `.openclaw/agents`, and `.openclaw/extensions` when those entries exist.
Symlinks, including dangling links, and other file types at those traversal boundaries are refused before copied-state rewrites or cleanup; the source stays unchanged and no target is registered.
Unconfigured agent directory links are still removed from the copy without changing the linked data.

While an environment upgrade can still replace or restore a runtime, new bindings to that runtime fail with a busy/retry error.
This includes environment creation, rebinding, cloning, importing, and restoring a snapshot with a different runtime binding.
Registered aliases also retain shared custody of the managed executable and symlink entries they depend on during binding admission and retained runtime reads.
Shared readers and unrelated runtime operations can proceed together; incompatible backing replacement or removal waits for that custody to end.
Retry after the upgrade finishes finalization, verification, and any rollback; unrelated runtimes remain usable.
On Unix, native upgrade children retain runtime ownership if the OCM parent exits, and release it when the children exit.

### Asynchronous upgrades

On Unix, an explicit operator request can run the ordinary environment upgrade in a detached OCM worker.
No extra permanent service is installed.

A job returns `up-to-date` without a checkpoint or restart when the requested package and binding are already current, package integrity and native configuration checks pass, and any running managed Gateway reports the same healthy build and the daemon confirms its actual launch specification matches the current desired settings.
Unknown build or launch identity, configuration needing repair, changed launch settings, and unhealthy or mismatched serving state keep the ordinary checkpointed upgrade path.
Older daemons without launch observations keep that existing behavior.
When an upgrade resolves an official release, older runtime records without archive integrity use the normal checkpointed install path to establish it.
Ordinary setup and repeat installation can still reuse a healthy matching legacy runtime, including one already bound to another environment.
Explicit target or track changes still run the upgrade, and direct managed-runtime `ocm upgrade` commands retain their repair and finalization behavior.

```bash
ocm upgrade job start mira --json
ocm upgrade job start mira --runtime tested-build --json
ocm upgrade job status mira --json
ocm upgrade job status mira --request-id <id> --json
```

These commands always return JSON.
`ocm upgrade job capabilities <env>` reports protocol version 1, platform support, selectors, the current `bindingKind` and `bindingName`, and the environment's root, state directory, and config path.
On Unix, supervised Gateways receive the spawning daemon's executable in `OCM_SELF` alongside their existing OCM environment identity; clients query capabilities to determine upgrade-job support.
Clients must use their trusted OCM executable and environment binding, and preserve their own requester authorization before submitting an operation.
Latest status returns `null` when that environment has no job; an unknown exact request ID is an error.
`start --request-id <id>` lets a caller choose a correlation ID before submission; repeating the same ID and target returns that existing job without replaying it.
Reusing an ID for a different target or a recreated environment is refused.
Clients acting on an observed binding can pass `start --if-binding <kind>:<name>`, copying `bindingKind` and `bindingName` from capabilities.
OCM rejects a mismatched new request before recording it and rechecks the condition under the environment operation lock before runtime or service I/O.
If the binding changes after admission while the worker waits, the accepted job fails without changing the binding, runtime, service, or upgrade history.
Repeating an accepted request with the same ID, target, and binding condition returns its original job even if that binding has since changed; changing the condition for that ID is refused.
Without `--if-binding`, explicit operator selectors retain their ordinary behavior, including conversion from a launcher to a packaged runtime.
Guarded requests use job-record version 2 so older workers refuse them instead of ignoring the condition; unguarded records retain version 1.
Latest status excludes completed jobs from an older environment instance; an exact ID still retrieves its original historical result.
An active prior-instance worker still blocks new jobs, and latest status reports that blocker with its exact request ID.
`start` accepts the same mutually exclusive `--version`, `--channel`, or `--runtime` selectors as a single-environment upgrade, with rollback enabled.
It returns only after the worker accepts the request, and the worker starts upgrade work after the response is written.
Existing environment and upgrade transaction locks still exclude conflicting mutations.
The worker verifies the admitted environment root and creation identity under the operation lock, before runtime or service I/O, and refuses a same-name replacement.
A second asynchronous request for the same environment is refused while the first is active.

Each response includes `id`, `envName`, `state`, `progress`, a monotonic `revision`, `createdAt`, a persisted `updatedAt` advancing at least one millisecond per revision, the requested `target`, and optional `result` or `error`.
The terminal `result` is the ordinary upgrade summary, including its precise `outcome`, target version, snapshot, and rollback facts.
A successful job can report an unchanged or skipped outcome; success alone does not mean a new release was installed.
A failed upgrade or completed rollback is reported as `failed`, with the original result preserved.
Request records live under the environment's upgrade-history directory and remain queryable by their original ID after later submissions.

If the worker disappears without recording a result, status reports `interrupted` and recovery remains unresolved.
That request is terminal and is never replayed.
After inspecting or repairing the environment and its recovery state, an operator may explicitly submit a fresh request ID through the ordinary upgrade checks and locks.
Known native writers that outlive the old worker retain environment exclusion, so the fresh operation waits for them to finish.
A fresh request does not resume the old transaction or change its recorded result.
An interrupted job does not establish that rollback succeeded or that retrying is safe.
This command is an explicit local operator action; it does not supply authorization revalidation for a remote requester or UI client.

### Upgrade checkpoint scope

By default, upgrade and rollback safety checkpoints cover the full environment.
To keep independent projects current across core upgrades and rollbacks, declare
their directories before the upgrade:

```bash
ocm env set-independent-paths mira .openclaw/workspace/projects .openclaw/workspace/worktrees
# Include separately located managed worktrees and a development checkout:
ocm env set-independent-paths mira .openclaw/workspace/projects .openclaw/worktrees development/checkouts
ocm env show mira --json
ocm env set-independent-paths mira none
```

The command replaces the list. Paths are relative to the environment root and
must name directories in one of these locations:

- strictly beneath a configured workspace;
- the managed-worktree content directory `.openclaw/worktrees`, or beneath it;
- a non-hidden top-level directory of the environment home, or beneath it.

Other hidden home/state directories remain ineligible. For example, `.openclaw/agents`,
`.openclaw/credentials`, `.openclaw/state`, and `.codex` cannot be excluded. The
managed-worktree registry and migration state remain in the state database, not
in the excluded checkout contents. These locations are eligibility rules only:
no content is excluded without an explicit declaration.

A declared directory may be absent if its parents exist. Parents must be real directories;
symlinks within an independent directory remain untouched. Individual files cannot
be declared independently, keeping SQLite databases and their adjacent WAL and
journal files together. Paths cannot overlap, escape the environment, contain
configuration or its includes, or encompass a configured workspace.

Only declare content that OpenClaw and its migrations do not own. Workspace memory,
identity, legacy state, and other meaningful files are not disposable. OCM cannot
discover every plugin or migration's write footprint, and this setting does not
sandbox runtime writes. Unclassified state remains covered, including credentials,
unknown runtime directories, and migration inputs outside the declared directories.

Preparation, stopped capture, and rollback validation do not traverse independent
directories. Restore replaces owned entries around them; it leaves current
independent bytes, modes, timestamps, extended attributes, symlinks, additions,
and deletions in place. Shared ancestor directories stay in place, but their
metadata can change when owned siblings are replaced. Existing service-log and
socket rules and SQLite validation still apply to the captured state.

Each upgrade checkpoint records its scope. Changing the environment's current
list affects future checkpoints only. Old whole-root checkpoints still restore
the whole root; declaring independent paths does not retrofit them. Scoped
checkpoints use a new kind that older OCM versions refuse to restore. Missing or
invalid scope metadata is rejected. Restore reverses completed moves if a later
move fails and retains displaced owned entries until service acceptance; it does
not add crash recovery for an uncatchable process or machine failure.

This policy belongs to the environment registration. Clone and import start with
an empty list. A separately requested full snapshot, export, or clone still
includes independent content. Full snapshot restore still rewinds independent
content without a registered or active temporary source owner, and the selected
environment's OCM-owned worktree, when captured inside the environment root.

Restores and required rollbacks refuse checkpoints whose recorded scope would
replace another named environment's registered dev source, active temporary dev
source, or required Git metadata, before service quiescence or restore staging.
Explicit rollback checks both the selected checkpoint and its failure-recovery
checkpoint. `--no-rollback` keeps its existing behavior. The check reads known registered and active temporary
source paths and Git identity metadata, including surviving history for a missing
worktree.
Checkpoint traversal still leaves independent content opaque; post-copy updates
and residue cleanup leave directory links untouched. Excluding only a worktree
is insufficient when its required Git metadata remains in scope.
Borrowed source and its known Git metadata also remain protected from a restore
of the borrowing environment itself. Missing borrowed paths stay reserved until
the binding is removed; an unrelated excluded sibling does not preserve that
reservation. Select a checkpoint whose saved exclusions cover the source and
all affected Git metadata. A temporary takeover keeps its source protected even
when the watching environment retains a runtime or launcher binding. Stop that
session with `ocm dev stop <watching-env>` before retrying a restore that would
replace its source. Once the session completes, its temporary protection ends;
a disjoint source does not block the restore.

### Snapshots

Snapshot restore and live upgrade or rollback require a completed dev session.
Request shutdown of recorded ownership with `ocm dev stop <env>` before retrying.
Older watches without an unfinished ownership record must be stopped from their
original dev terminal. Unreadable or unverified ownership requires verified
operator recovery: preserve the environment
and check the watch processes and service policy before recovering its ownership
record. Refusal preserves the current state and service policy.

Manual restore of a dev environment keeps its current source binding and
runtime/launcher binding while restoring saved state. Upgrade rollback keeps
that dev identity but restores the recorded runtime/launcher. Ordinary runtime
and launcher environments still restore their captured bindings.

Snapshot create and restore stop a running OCM-managed gateway before copying or
replacing its root, then restore the recorded service policy. New snapshots are
verified whole-root checkpoints: they include secrets, browser state, SQLite
sidecars, modes, symlinks, plugin data, and unknown future paths. Restore stages
an exclusive candidate beside the live root and retains the displaced root
until service acceptance; failed acceptance restores the displaced root.

Unix sockets are transient process endpoints and are omitted from checkpoints,
including inactive socket files left by stopped processes. Snapshot creation
leaves source endpoints untouched. Restore does not recreate sockets; their
owning processes create them when needed. This rule uses the entry's file type,
so ordinary files named `*.sock` and symlinks remain part of the checkpoint.
Other unsupported special entries, such as FIFOs and devices, are rejected
during preparation before a running gateway is stopped. Final capture also
checks for unsupported entries introduced after preparation.

APFS checkpoints begin as space-efficient copy-on-write clones. Other
filesystems require a full metadata-preserving copy. Plan space for checkpoint
divergence and a temporary restore candidate. These same-disk, secret-bearing
checkpoints are operational rollback—not off-disk disaster recovery. Legacy tar
snapshots remain readable.

Create:

```bash
ocm env snapshot create mira --label before-upgrade
```

List:

```bash
ocm env snapshot list mira
ocm env snapshot list --all
```

Show:

```bash
ocm env snapshot show mira <snapshot>
```

Restore:

```bash
ocm env snapshot restore mira <snapshot>
```

Restore retains the displaced environment until supervisor synchronization and managed-service acceptance succeed.
If synchronization fails, OCM attempts to restore the pre-restore root and registry before returning the error. A quiesced service resumes only when its prior data was unchanged or recovered successfully, including recovery with cleanup or resynchronization warnings.
An error distinguishes a failed rollback from restored data that still needs supervisor resynchronization or cleanup.
If rollback cannot finish, OCM withholds automatic service resumption and pauses running intent while verifying that the managed service is quiescent. A later synchronization or observation failure does not restore running intent; idle daemon cleanup is deferred. A loaded daemon must have a verified definition path, and a running daemon must provide a runtime record identifying its current process before OCM can confirm quiescence; a missing, legacy or stale identity is reported as unverified recovery. The error identifies any stop failure, the retained restore operation directory and the failed recovery step; inspect the active environment and the retained `displaced` and `rejected` trees before retrying, and preserve that directory until recovery is verified. Start the service explicitly only after verifying its recovered root, registry binding and configuration.
A stopped or absent daemon does not prove its previously admitted children stopped. Recovery retains uncertainty while the target has child or service records, including a terminal record that the daemon never retired; records for unrelated environments do not prevent target quiescence. These records are not proof of current process identity, so do not signal a process using a retained PID alone. Independently verify target shutdown and recovered data before restarting; OCM does not automatically settle orphaned children or erase retained ownership during recovery.
If only cleanup fails after acceptance or rollback, the reported directory contains leftover operation data; remove it only after verifying the active environment and any needed recovery copies.
These directories can contain credentials and other private environment data.

Remove:

```bash
ocm env snapshot remove mira <snapshot>
```

Prune:

```bash
ocm env snapshot prune mira --keep 3 --yes
```

### Export and import

Export:

```bash
ocm env export mira
ocm env export mira --include-state --output ./mira-with-state.ocm-env.tar
```

Import:

```bash
ocm env import ./mira-with-state.ocm-env.tar --name rowan
```

Export omits `.openclaw/state` by default. `--include-state` means "export this
environment as the same installation": it includes every file under the
environment root, including plugin payloads and dependencies, SQLite-backed and
file-based sessions, and unrelated or legacy data. Only non-durable process
files and rebuildable runtime debris are excluded, using the snapshot rules
for sockets, pids, locks and temporary directories, plus extension dependency
caches and workspace runtime debris. Each SQLite database anywhere under the
root is captured as a coherent committed snapshot, including uncheckpointed WAL
writes; raw database sidecars are omitted. Databases and other files are not
captured in one atomic transaction.

Default export accepts configured workspaces under an aliased environment root using either the accepted root spelling or its canonical path.
On Windows, ordinary and extended-length prefixes are equivalent representations of those same roots.
Equivalent spellings select the same owned content and omit each workspace's `.openclaw/tmp` runtime debris.
Other root spellings retain the default export refusal, even when another alias resolves inside the environment.
External workspaces, the environment root itself and workspace paths redirected by internal symlinks remain refused.

New archives, with or without `--include-state`, retain the accepted source root and its captured canonical path in optional `env.canonicalSourceRoot` metadata.
Configuration relocation uses both recorded roots without requiring the original source or alias to exist; state-inclusive runtime-reference relocation uses the same identities.
Older archives retain their existing source-root matching rules.
The archive format remains version 1, and older readers ignore the optional field.
Source identities may use POSIX, Windows drive, or UNC syntax; the extension preserves acceptance of portable foreign-platform contents without adding translation of platform-specific payload paths.
On Windows, ordinary drive/UNC and extended-length spellings of a recorded root relocate together.

Default archives can still retain SQLite sessions in selected agent directories, using current `session_nodes` or legacy `session_entries` storage.
Import uses the available recorded source identities to relocate their authoritative workspace fields, including databases in configured agent directories, without adding the global state database to the archive.
Other session JSON, authored content, Board checksums and grants, and integrity-protected history remain unchanged.
A configured active database inside protected history causes import to fail and removes the partial target before registration.
This ownership check also applies to older archives without source-root metadata; unrelated history remains unchanged.

State-inclusive export copies configuration files verbatim. Import rewrites
env-scoped OpenClaw config paths for the new root, including agent workspaces
and agent directories, through the same rules as an ordinary import. It also
assigns the new gateway port and resets the sandbox origin. Configuration must
be valid; includes that own agent workspaces or the sandbox origin must be
flattened first, as for ordinary imports. No OpenClaw runtime is required.
State is preserved; references to the environment root are rewritten to the
new root. Import rewrites text values across registered OpenClaw SQLite databases,
including session metadata, recorded file paths, worktree records, and plugin
receipts, plus the runtime JSON/JSONL and text files handled by plain-home migration.
It retains sessions, logs, leases, and other durable state. Unchanged values and
binary payload columns (including compressed records) retain their bytes. Other
workspace files and integrity-protected history also retain their bytes.
Import refuses a selected active database that resolves to protected history and removes the partial target before registration, instead of preserving stale runtime state or changing historical bytes.
Unselected history and ordinary unprotected database aliases remain supported.
The ownership check also applies when restoring to the original root or importing an archive without source-root metadata.
Authored Board tabs and widgets retain their content, checksums, and associated metadata unchanged, even when their text mentions the original environment root.
Every other file outside the config and state-reference rewrites stays byte-identical to the archive.
Migration and state-inclusive import share root-reference matching for plain
paths, file URLs, and existing runtime text. A root must end at a path component
boundary; sibling names that share its prefix remain unchanged. On POSIX,
backslashes in filenames are not directory separators. Free-form text uses literal
root substitutions without interpreting command syntax or resolving `..` segments;
structured path fields and file URLs are checked as complete paths. Within text,
file URLs end at a closing quote or angle bracket, a control character, or the next root
reference; spaces alone stay within the URL. References
without the source-root prefix are left unchanged. External contents are not
exported and may need separate preparation after moving.
Import refuses to update a database outside the copied root; internal database
links are retained.
Owned file and directory links use relative targets in the archive; external
and dangling links retain their targets without copying external contents.
Import relocates selected Git repositories and linked worktrees to the destination before registering the environment.
This includes native bare repositories (`git init --bare`): source-local configuration includes resolve to their retained destination files in both archive modes.
Their required Git storage must be present inside the copied archive; an incomplete graph or unresolved copied lock fails import and removes the incomplete target without modifying the archive or source.
Git indexes, branches, snapshot refs, authored files, and protected history retain their contents.
Ordinary-import cleanup retains the selected configuration files and links required by those Git repositories.
Use `--include-state` when managed-session registry continuity is needed: ordinary archives still omit `.openclaw/state/openclaw.sqlite` and only retain their existing selected files.

State-inclusive archives are created with private access. Import preserves
directory modes under a private environment root and leaves it stopped;
export leaves the source unchanged.
Archive ownership and extended ACLs are not transferred; the private root
prevents broader access when those restrictions cannot be reproduced.
An explicit `env import --sandbox-origin` still replaces that setting.

Export JSON reports `includeState`; its human summary also reports whether
state was included. State-inclusive import reports `configPathsRewritten: true`
and `statePathsRewritten` in JSON; the latter says whether any state references
changed. `databasePathsRewritten` maps each processed database path relative to
the imported root to its count of changed text values; zero means no values
changed. The human summary includes these counts. Export without the
flag and imports without included state retain their existing selection,
configuration rewriting, and cleanup behavior.

Environment archives must contain real `meta/` and `root/` directories and a
regular `meta/env.json` file. Import and legacy snapshot restore reject symlinks
at those entries before reading metadata or copying environment contents, so
an archive cannot redirect those operations to files outside its extracted tree.

### Rebind a borrowed development source

Use an explicit target to retain a stopped preview's data while changing its code:

```bash
ocm dev stop luna
ocm dev rebind luna --repo /path/to/next-openclaw
ocm dev luna --repo /path/to/next-openclaw
```

Stop any restored or independently running background service with
`ocm service stop luna` before rebind. The old checkout need not exist. The new
target must be a valid main or registered linked OpenClaw Git checkout, isolated
from the environment's deletable state and Git metadata. Concurrent mutations,
active or unfinished foreground ownership, unsafe background ownership and
occupied Gateway ports are refused without changing the binding.

Rebind only updates the source binding of an existing borrowed dev environment.
It preserves conversations, configuration, workspace, credentials, reserved
Gateway/UI ports and service policy; it does not install, start, stop, clone, seed
or edit either checkout or user state. Prepare dependencies and build artifacts
in the selected checkout before normal dev startup. User/plugin-owned settings
and external plugin paths are preserved, not relocated. Legacy OCM-owned sources
and runtime or launcher environments cannot be converted by this command.

`--json` returns `envName`, `previousSourceRoot`, `sourceRoot` and `changed`.
Repeating the same canonical target validates ownership and checkout identity
but leaves registry bytes unchanged and returns `changed: false`. A validation
or atomic-publication failure preserves the previous binding. Ordinary `dev`
and `--force` never switch sources. One-shot execution and status use the new
binding immediately; the next explicit service start rebuilds its launch plan,
and admission rejects a saved plan that still names the old source.

### Inspect and repair

```bash
ocm env status mira
ocm env doctor mira
ocm env cleanup mira --yes
ocm env repair-marker mira
```

Runtime-state cleanup preserves configured agent directories inside the environment, including their authentication and state files beneath generated plugin dependencies.
It still removes stale sessions and unselected generated files beside those directories.
Configuring an agent directory above `agents/<id>/sessions` preserves agent payload beside that subtree without hiding its stale sessions from cleanup.
If an agent directory is exactly `agents/<id>/sessions`, cleanup preview and apply refuse the conflicting ownership before changing any configuration or data.
Use a separate agent directory before retrying cleanup; `env doctor` reports the conflict.
External configured agent directories remain untouched and do not prevent this cleanup.
Run `ocm env cleanup mira --json` to preview the actions before applying them with `--yes`.

### Remove or destroy

Remove only the environment:

```bash
ocm env remove mira
```

Destroy the environment, uninstall its environment service, and remove its snapshots:

```bash
ocm env destroy mira
ocm env destroy mira --yes
```

`environment service uninstalled: <env>` confirms that the environment was
removed from OCM service supervision. It does not report deletion of the shared
daemon: a running sibling keeps that daemon, and it may already be absent after
stopping the last running environment. JSON `serviceUninstalled` retains this
environment-service meaning; `serviceLabel` remains `ocm`.

Destroy first stops the recorded foreground dev session, including any required
service restoration, and verifies that its owned processes exited. It then
rechecks the environment binding and protection before removing state. A
replacement session or changed binding prevents removal. While a watch is
running, the preview reports deferred process inspection; the remaining process
tree is inspected after the watch stops.

For new Unix foreground generations, unverified output or process completion keeps
ownership recorded. A controller crash, raw signal, or missing output EOF cannot
be cleared by repeating stop, starting another watch/service, or destroying the
env.
Normal acknowledged errors remain retryable; released legacy watch records and Windows process-job recovery retain their process cleanup rules.
Recovery compares the takeover revision saved by newer sessions before restoring the service; a later Stop or Uninstall remains authoritative.
If a controller dies during takeover before its resulting policy revision is saved, recovery preserves the current policy.
Older session records without that revision retain their existing recovery behavior because their historical policy observation is unavailable.
Use a compatible OCM CLI and refresh an older running daemon before creating a new generation.

If a stopped controller retained a cleanup failure, first independently verify
that every source process, including detached workers, has stopped. Then run
`ocm dev stop <env> --acknowledge-stopped-processes` to release that failed
session. An empty recorded process group alone does not establish that detached
workers stopped. OCM refuses recovery while its controller, recorded children,
process groups, or lease are active, or when child ownership is unpublished or
the environment/process scope changed. Recovery preserves the checkout, config,
and current service policy; it does not signal processes or restart a service.
`--json` retains the stop summary with `serviceRestored: false`. After recovery,
normal dev retry and environment removal are available. Start the background
service separately if wanted.

`dev <env>` owns setup, the native Gateway watcher and Vite UI by default.
`--no-watch` uses `scripts/run-node.mjs` instead of `scripts/watch-node.mjs` for
the Gateway; `--no-ui` disables Vite. `dev stop <env>` stops the recorded session
without removing its environment or source. Repeating the same source, launch
endpoint and effective backend watching/UI choices reuses the session; changing
those choices requires stopping it first. `dev status --json` reports active
ownership separately from `sourceWatch.watching`.
`--force` temporarily takes over a running background service while backend watching is enabled and restores it on exit only if no later service policy request supersedes the takeover; it rejects `--no-watch` or `--service`.
Temporary source sessions also refuse to start while an environment operation
can replace their checkout or Git metadata. Retry after the containing operation
finishes; refusal leaves the temporary session and service policy unchanged.
Operations on disjoint sources do not block admission.
A successful `service stop` or `service uninstall` during preparation or the foreground session remains authoritative after cleanup, including a Stop that repeats the temporarily stopped state.
In that case, `dev stop --json` reports `serviceRestored: false`; an unchanged takeover still restores the running service, and an initially stopped environment remains stopped.

Named `dev status` and JSON/raw output also report Gateway `/health` responses
and the captured UI process and HTML document separately. JSON includes
`gatewayHealthReady`, `ui.processRunning`, `ui.httpReady`, and `ui.issue`;
unverifiable UI ownership keeps readiness `null`. The existing `uiUrl`/`ui_url`
address remains available for active UI sessions. These observations use bounded
loopback requests and leave recorded state unchanged.

`dev <env> --service` records its preparation children until verified completion.
Use `dev stop <env>` to cancel setup; a managed Gateway already running keeps its
launch plan, and deferred restart requests remain pending. Explicit service stop
or uninstall still wins. After setup, OCM closes preparation and rechecks the
same environment, source binding, and service policy under one operation lock
before starting the managed service. An unchanged rerun keeps the running service.

During foreground dependency installation, pnpm lifecycle reports distinguish a
completed installation error from interrupted or unfinished build scripts. On
Unix, uncertain script cleanup retains ownership even when pnpm returns a normal
error or allows an optional build to fail. Completed errors remain retryable.
When run from a terminal, installation keeps interactive stdin while streaming
build output and diagnostics. Onboarding keeps its real terminal output.

For direct dev-source commands, OCM disables Node's module compile cache before
launch by setting `NODE_DISABLE_COMPILE_CACHE=1` and removing `NODE_COMPILE_CACHE`.
This avoids the launcher's compile-cache bootstrap wrapper without changing
`NODE_OPTIONS`, native runner selection, rebuilds or auto-doctor. The policy is
scoped to source execution; ordinary runtime and launcher environments retain
their existing Node settings.

`env remove` and `env prune` require active or unfinished source watches to be
stopped first with `ocm dev stop <env>`. The same applies to a destroy guarded by
`--if-state-token`: stop the watch, then request a fresh preview. A watch created
by an older OCM without usable stop ownership must be stopped from its original
terminal. Environment removal clears completed watch records and retains the
reusable synchronization lock files.

Cleanup refuses to remove another registered dev source or its required Git
metadata, including sources reached through path aliases. Destroy checks before
signalling source workers; simulation checks before discarding generated files.

`destroy` is the stronger cleanup path.

## Updating `ocm` itself

Check for updates:

```bash
ocm self update --check
```

Update to the latest release:

```bash
ocm self update
```

Update to a specific version:

```bash
ocm self update --version 0.2.1
```

If you installed `ocm` long ago and the `self` command is not available yet, rerun the install script once with `--force`, then use `ocm self update` from then on.

## Output modes

By default, `ocm` formats output for people in a terminal.

Use:

- `--json` for structured output
- `--raw` for plain output
- `--color auto|always|never` for explicit color control

Examples:

```bash
ocm service list --json
ocm env status mira --raw
ocm --color always runtime list
```

## Command map

### Top-level commands

- `ocm setup`
- `ocm start`
- `ocm self`
- `ocm env`
- `ocm release`
- `ocm runtime`
- `ocm launcher`
- `ocm service`
- `ocm init`
- `ocm help`
- `ocm --version`

### `env`

- create, clone, list, show
- use, exec, run, resolve, status
- set-runtime, set-launcher
- doctor, cleanup, repair-marker, protect
- snapshot create, list, show, restore, remove, prune
- export, import
- remove, destroy, prune

### `release`

- list
- show
- install

### `runtime`

- add
- build-local
- list
- show
- install
- update
- releases
- verify
- which
- remove

### `launcher`

- add
- list
- show
- remove

### `service`

- list
- status
- discover
- logs
- install
- start
- stop
- restart
- uninstall
- adopt-global
- restore-global

### `self`

- update

## Platform support

Current support:

- macOS
- Linux

Background services:

- macOS uses `launchd`
- Linux uses `systemd --user`

Windows service support is not implemented yet.

On macOS, service ownership is read from `EnvironmentVariables.OCM_HOME` in
the LaunchAgent plist. Plist-aware editors can reformat the definition or
convert it to binary without changing ownership. A different, missing, or
invalid owner still prevents OCM from replacing or controlling that service.

## Safety notes

`ocm` keeps safety checks around destructive actions.

On Unix, updating an already-private OpenClaw config preserves its file mode.
On Windows, this preserves a protected ACL owned by and granting access only to
the current user. Private replacements exclude inherited macOS and Windows access
at creation, and protection is verified before any config data is written. Other
authored access rules retain the existing writer. Config values, including auth
and SecretRefs, follow the existing command's update rules.

Examples:

- protected environments are not removed unless forced
- environment roots are marker-checked before destructive cleanup
- preview-first behavior is used where it matters
- service install picks the next free port instead of colliding with an existing one

## Where to look next

- `ocm help`
- `ocm help start`
- `ocm help setup`
- `ocm help env`
- `ocm help release`
- `ocm help runtime`
- `ocm help launcher`
- `ocm help service`

## Retained service ownership recovery

`ocm env status <env>` also reports `openclawState: "unknown"` while recorded service ownership remains unresolved, even when service intent is stopped or its binding is missing.
Its terminal guidance points to `ocm service recover <env>` to inspect the claim before starting or repairing the environment.
This status does not acknowledge recovery or change service intent.

A replacement background daemon preserves unresolved runtime children and service rows from its predecessor, including pending rows whose child PID was never published. It refuses to start another child for those targets while allowing unrelated environments to progress. Service status reports retained ownership as `gatewayState: "unknown"`, not a verified running or stopped process. An old listener answering a health check does not make a replacement Start succeed. Start waits for a runtime observation whose process identity matches the current daemon, including before that daemon first publishes. If no current observation arrives within the readiness timeout, Start reports that it is not ready and provides the target's `ocm service start` command to retry after the supervisor finishes starting. Snapshot capture and restore, service restart, and foreground dev startup also refuse an unresolved target claim, including after Stop or native daemon exit. Exact recovery releases that restriction for the target without blocking unrelated environments.

First stop the target's running intent and inspect the claim:

```sh
ocm service stop mira
ocm service recover mira
ocm service recover mira --json
```

Independently inspect and stop every process belonging to that old workload, including detached workers and any child whose PID was never published. Then use the token printed by that inspection:

```sh
ocm service recover mira --acknowledge-stopped-processes --if-claim <token>
```

The acknowledgement is your assertion that those processes stopped; OCM does not certify process absence. Stop must also have removed the target from the persisted supervisor plan; an incomplete policy synchronization remains a refusal. It rejects visible conflicts: running intent, a live recorded PID or process group, a live original publisher, missing publisher identity, changed process scope, a changed claim/environment, an incompatible live daemon, or an observation failure. A reused PID is a conservative veto; OCM never signals it. Empty process observations do not replace your confirmation, especially for pending records or detached descendants.

Recovery releases only the inspected claim. It preserves environment data, current stopped intent (including after an incomplete restore), and unrelated siblings. It never starts or stops the native daemon, kills processes, or starts a service. The same command works with a compatible live replacement or an absent daemon whose recorded publisher is conclusively gone. A changed token requires a fresh inspection. JSON reports `operatorAcknowledged: true`, `released: true`, `processAbsenceVerified: false`, and `desiredRunning: false` on success. Start the service separately only when wanted and after any failed data recovery has been resolved.

Existing runtime fields remain readable; the optional `retainedOwnership` metadata preserves each inherited claim's identity and original publisher. Running older daemons cannot perform this recovery. Running an older daemon again can reintroduce its ownership-loss bug; downgrade safety is not provided by the new metadata. Records without usable publisher identity or with a foreign scope remain refused. An unreadable ownership record also remains a refusal when the native daemon is stopped or absent; preserve the damaged record rather than treating it as empty ownership. A no-claim result does not establish absence for a legacy child that was never recorded. Foreground `dev stop --acknowledge-stopped-processes` owns a separate session and cannot release a background claim.
