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

Saved dev service plans must still match the env's current binding and recorded
worktree. If OCM refuses a stale plan, use `ocm service restart <env>` to refresh it.

Normal restart is gateway-aware when `ocm service status mira` reports restart
handoff `protocol v1`: OpenClaw records eligible active sessions and subagents,
hands the fresh-process restart back to OCM immediately, and resumes recoverable
work after the replacement gateway starts. The old gateway process does not wait
for an in-flight turn to finish.

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

### Clone an environment

```bash
ocm env clone mira rowan
```

Clone copies the workspace and env config into a new environment, gives the clone its own gateway port, rewrites env-scoped OpenClaw config paths under the new env root, keeps durable agent auth/settings for the same user, clears copied runtime residue like sessions, logs, and backups, and keeps the background service separate. Clone does not copy dev source bindings. The usual next step is:

```bash
ocm start rowan
```

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
includes independent content. Full snapshot restore still rewinds unregistered
independent content and the selected environment's OCM-owned worktree when those
paths were captured inside the environment root.

Restores and required rollbacks refuse checkpoints whose recorded scope would
replace another named environment's registered dev source or required Git
metadata, before service quiescence or restore staging. Explicit rollback checks
both the selected checkpoint and its failure-recovery checkpoint. `--no-rollback`
keeps its existing behavior. The check reads only known registered source paths
and Git identity metadata, including surviving history for a missing worktree.
Checkpoint traversal still leaves independent content opaque; post-copy updates
and residue cleanup leave directory links untouched. Excluding only a worktree
is insufficient when its required Git metadata remains in scope.
Borrowed source and its known Git metadata also remain protected from a restore
of the borrowing environment itself. Missing borrowed paths stay reserved until
the binding is removed; an unrelated excluded sibling does not preserve that
reservation. Select a checkpoint whose saved exclusions cover the source and
all affected Git metadata.

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
```

Import:

```bash
ocm env import ./mira.tar --name rowan
```

Imported environments get a fresh identity, have env-scoped OpenClaw config rewritten for the new root, and keep durable agent settings while clearing copied runtime residue like sessions, logs, and backup files.

### Inspect and repair

```bash
ocm env status mira
ocm env doctor mira
ocm env cleanup mira --yes
ocm env repair-marker mira
```

### Remove or destroy

Remove only the environment:

```bash
ocm env remove mira
```

Destroy the environment, its OCM-managed service, and its snapshots:

```bash
ocm env destroy mira
ocm env destroy mira --yes
```

Destroy first stops the recorded foreground dev session, including any required
service restoration, and verifies that its owned processes exited. It then
rechecks the environment binding and protection before removing state. A
replacement session or changed binding prevents removal. While a watch is
running, the preview reports deferred process inspection; the remaining process
tree is inspected after the watch stops.

For new Unix foreground generations, unverified output or process completion keeps
ownership recorded. A controller crash, raw signal, or missing output EOF cannot
be cleared by repeating stop, starting another watch/service, or destroying the
env. Normal acknowledged errors remain retryable; released legacy watch records
and Windows process-job recovery retain their existing rules. Use a compatible
OCM CLI and refresh an older running daemon before creating a new generation.

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
ownership separately from `sourceWatch.watching`. `--force` temporarily takes
over a running background service while backend watching is enabled and restores
it on exit; it rejects `--no-watch` or `--service`.

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
