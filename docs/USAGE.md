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
Minimum setup fills missing settings using the effective configuration, preserving
authored values supplied through supported internal `$include` files. Included
files remain unchanged, and the environment’s selected Gateway port still applies.
Minimum setup inspects only the settings it needs; unrelated include branches are
left to OpenClaw and are not validated by this setup step.
To use a shared include directory, supply `OPENCLAW_INCLUDE_ROOTS` to OCM; its
platform-separated directory list is admitted to both minimum setup and the
launched OpenClaw process. Relative entries use the configured Gateway working
directory, and `~` uses the environment's OpenClaw home. Foreground `dev` uses
its recorded source checkout for relative roots, even when the environment
retains a runtime or launcher default. Ordinary `start` and `dev --service`
continue to use the selected default binding's working directory. Config `env` cannot
authorize additional roots. If a required minimum setting depends on an include
outside those roots, setup stops before writing configuration or launching a
Gateway. Expose the root authorization to OCM or author that setting in an
observable file; roots known only inside an opaque launcher cannot be inferred.
This read-only permission does not relax clone/import/export containment.
With `--onboard`, onboarding runs first, and OCM installs and starts the service only
after onboarding succeeds. This also applies when you choose both onboarding and
background running in `setup`. Failed or cancelled onboarding leaves a new
environment stopped and preserves an existing environment's service state. Retry
with `ocm start <name> --onboard`; use `--no-service` to finish onboarding without
starting a background service. Gateway readiness is still checked when OCM starts
the service after onboarding.
When `start` moves an existing environment from its runtime or launcher to a
different runtime with `--runtime`, `--version` or `--channel`, it uses the same
transition as `ocm env set-runtime`: OCM refuses a downgrade, captures a
pre-upgrade snapshot, runs OpenClaw's update finalization before the service
restarts, and restores the snapshot and previous binding if the transition
fails. Starting on the current runtime and launcher changes rebind directly.
Repeating `ocm start luna --command ... --cwd .` from the same directory reuses the local
launcher, as does the equivalent absolute directory. A different command or
directory is refused without changing the existing launcher. When `start` is
refused before it creates or rebinds the env, it removes the launcher it just
created, so a corrected retry starts like a first attempt.

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
borrower keeps its canonical source path. Starting a stopped borrower with a
different explicit or enclosing checkout switches it there under the checks of
`ocm dev rebind`, preserving its state, prints the previous and selected
checkouts, and continues the start. A running, starting, unfinished or busy
borrower, or one whose Gateway ports are occupied, is refused with its binding
unchanged. Borrowers with a runtime or launcher binding and existing OCM-owned
environments retain their recorded sources and original repository selection.

New borrowers, and borrowers just switched to another checkout, may prepare
missing tooling with `pnpm install --frozen-lockfile`. If a switched start fails
or is cancelled before the selected checkout is prepared, its processes are
verified stopped and the previous checkout is still valid, OCM switches the
environment back, so the next start from the selected checkout switches and
prepares it again. Rollback applies only to the original environment generation;
an environment removed and recreated under the same name keeps its own source
binding. Once the checkout is prepared the switch stays, even if the Gateway then
fails to start; the next start there needs no preparation.
Otherwise the new binding stays and OCM says so: an installer stopped by a
signal leaves cleanup unverified, so its session ownership is kept for recovery,
and a missing previous checkout cannot be restored. Missing tooling in the
selected checkout then needs explicit preparation.
Before automatic installation, OCM reads the configuration the selected pnpm
reports (such as `pnpm-workspace.yaml`, pnpm's global `config.yaml` and
`PNPM_CONFIG_*` settings) and refuses linked modules or virtual-store
destinations: each lockfile project's modules directory, the lockfile root's
modules and installation state, custom project and global stores, a global
store kept under pnpm's store directory, and linked parents. It also refuses a
pnpm workspace-directory override (`PNPM_CONFIG_WORKSPACE_DIR`,
`NPM_CONFIG_WORKSPACE_DIR` or their lowercase spellings) that names another
directory, and per-project pnpm lockfiles. Failed configuration readback also
requires explicit preparation; physical default and custom destinations remain
supported. Destinations chosen by a checkout's pnpmfile hooks or configuration
plugins during installation are not checked, because pnpm applies them only
while installing.
Other resumed borrowers report missing tooling for explicit preparation; they do
not reinstall dependencies. Removing a borrower preserves source files, dependencies,
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
Dev UI preflight reads its Gateway settings with the same explicit
`OPENCLAW_INCLUDE_ROOTS` authorization as the selected source launch. Relative
roots use that source checkout, and `~` uses the environment's OpenClaw home.
Config `env` cannot grant include access; unrelated config branches remain for
OpenClaw to resolve. TLS and disabled-Control-UI guards still apply.
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
`--no-ui` for TLS or disabled-UI environments. A configured in-checkout modules
directory is accepted before native startup creates its root `node_modules`
link, including pnpm UI package links through that root. OCM checks these
dependencies without creating the link or accepting packages from another
checkout. When the link is absent, the UI waits for native source startup to
create it; cancellation, source exit, or a 30-second timeout ends that wait.
Suspending the foreground source also suspends OCM and pauses that startup
timeout until resume.
`--service` uses the background
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

Packing scratch belongs to the runtime's installation stage. Normal completion
or failure removes it; after an interrupted build, a later installation or
removal of an existing runtime reclaims abandoned stages. Active runtime
operations remain protected. Packing and installation children retain custody
of their stage if OCM exits before they do, so recovery waits for them to end.
Legacy `.pack-*` directories from older OCM versions are left untouched because
their ownership cannot be established safely from their names alone.

When the root package or selected source extensions depend on private
`workspace:*` packages, OCM resolves
their complete transitive closure. Its build-only npm proxy rewrites nested
workspace specs to exact local versions only inside scratch archives, leaving
the selected checkout unchanged.

Repeat `--companion <plugin-id>` when the checkout contains an official plugin that is version-bound to the selected OpenClaw build but published separately. OCM uses OpenClaw's package-local plugin release contract, requires the plugin package version and `openclaw.build.openclawVersion` to match the root package exactly, installs its dependencies in an isolated runtime-owned tree, and stages it under `dist/extensions/<plugin-id>` so OpenClaw discovers it as bundled. The runtime record stores the package version plus artifact and entrypoint hashes, and `runtime verify` checks the installed entrypoint hash. OCM refuses missing, mismatched, non-publishable, duplicate, or already-bundled companion selections before publishing the runtime.

Companion materialization retains npm-installed dependency bindings across hoisted, nested, and linked layouts without changing the selected npm strategy. Private dependency versions remain private, and the explicit OpenClaw peer uses the host runtime. Dependencies survive removal of packing scratch, and shared packages retain their module identity. On Windows, OCM uses directory junctions when symbolic-link privilege is unavailable, preserving these bindings through publication and runtime recovery. A binding that the filesystem cannot preserve fails the owned build before publication instead of substituting a separate package instance. `runtime verify` checks recorded integrity; after enabling a companion, use `ocm @<env> -- plugins inspect <plugin-id> --runtime --json` and inspect its status and diagnostics to verify loading.

On Windows, companion verification recognizes native separators in generated runtime records. It still requires a relative path within the selected companion and checks package metadata, file existence, and entrypoint integrity.

On Windows, internal package junctions remain bound to the prepared installation
until commit, then move with the published runtime. Integrity metadata covers the
published link targets. Runtime recovery checks saved package bytes independently
of the current installation and preserves the recorded bindings for restoration.

Companion installation also supports an existing `OCM_HOME` directory alias with npm's linked strategy. OCM resolves the owned npm prefix without changing the caller's strategy or the store path in runtime metadata. Failed companion installation does not publish a partial runtime.

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
Materialized extensions retain the installed dependency bindings, including
when npm hoists a version that differs from the core package's private version.
These builds also support an existing directory alias in `OCM_HOME`, including
native npm's `linked` installation strategy, without changing the caller's npm settings.
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

OCM reads the target environment's effective plugin policy before `npm pack`, validates
explicit plugin references against the checkout and installed-plugin records,
including the native SQLite index, inline config and legacy JSON records,
and follows transitive local plugin package dependencies. Policy IDs are trimmed
and compared without case distinctions. Aliases declared by the selected source
manifests (provider, channel, setup, CLI backend, model catalog, provider auth,
and legacy plugin IDs) resolve to their canonical plugin IDs across allow,
deny, entries and slots. Canonical source and installed identities take
precedence over manifest aliases; shared aliases use native plugin ID ordering.
OCM also recognizes the native built-in
aliases `google-gemini-cli` → `google` and `minimax-portal` /
`minimax-portal-auth` → `minimax`. An exact declared source ID takes precedence,
so older checkouts keep their legacy identity even if a canonical installed
record exists. Installed legacy IDs remain recognized when their canonical
replacement is unavailable. Authored config and declared manifest IDs remain
unchanged; unknown enabled references still fail validation.

Policy reads honor `OPENCLAW_INCLUDE_ROOTS` explicitly supplied to OCM, using
that target's startup environment and working directory (including launcher or
dev source directories for relative roots). They read plugin policy and, only when its values use environment templates,
its config environment inputs, without inferring roots from launcher shell commands
or `config.env`, reading unrelated configuration branches, or copying shared
fragments. Clone, import, and export retain their existing containment rules.

Plugin policy string values resolve supported `${VAR}` templates after includes
are loaded, using the target environment context and config `env`/`env.vars`.
Nonblank process values take precedence over config-provided values. Object
keys are not substituted, and unresolved required references still fail availability
checks. This inspection does not load additional `.env` files or shell
environments. Authored config stays unchanged, and failures hide resolved
policy values rather than printing potentially sensitive environment contents.

Installed external plugins satisfy that validation without being bundled into
the new runtime; installed records do not select additional plugins. Disabled
entries and denied plugins remain excluded even when their identifiers remain
in `plugins.allow` under a supported alias or case variant, including when the
plugin is no longer installed. Disable and deny references use the same
selected-source identity rules. Alias/case-equivalent entries share enablement:
a later object without `enabled` preserves the previous value, while a later
explicit boolean replaces it. Explicit slot references still require the
selected plugin to be available, except the native built-in `contextEngine: "legacy"`
and case-insensitive `"none"` disabled values. The `legacy` exemption applies only
to the context-engine slot. The resulting runtime adds only required
source plugins omitted from the core package. `--for-env` and
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
- local repo targets install dependencies with `pnpm install --frozen-lockfile` in the simulation's own worktree, after the same pnpm destination checks `dev` applies; a linked destination, a modules-directory override outside the worktree or a workspace-directory override naming another directory fails the `prepare target` check before anything is installed
- simulation envs and unused temporary runtimes are cleaned up automatically; use `--keep-simulations` only when you need retained debug artifacts
- SIGINT (Ctrl-C) or SIGTERM while the target runtime is still downloading stops OCM at once without registering it; after the runtime is registered, a signal lets the check in progress finish, skips the remaining checks and scenarios, cleans up as above, reports a failed `interrupted` check, and exits with status 1
- each simulation command checks the original clone's registration before launch; removing or recreating that registration makes remaining checks fail instead of sending commands to a replacement. Rerun `ocm upgrade simulate` against the source environment to create a fresh clone. An already running check may finish, and `--keep-simulations` does not bypass command admission
- cleanup removes only the simulation environment generation created by that run; if the name was removed and recreated during checks, the replacement is preserved and cleanup reports failure without changing the functional outcome
- if another environment binds to a temporary runtime, cleanup preserves that runtime and reports `cleanup: "failed"` with the dependent environment names; the functional `outcome` is unchanged, and the command exits nonzero. Clear those bindings before removing the retained runtime with `ocm runtime remove <name>`
- runtime cleanup also checks the registration created by the simulation. If it was removed and its name independently reinstalled, cleanup preserves the replacement and reports failure, including when target preparation failed before binding. A runtime update keeps the same registration lifetime and does not prevent cleanup of an unused temporary runtime. `createdAt` stays fixed across updates and forced replacements of a readable registration; `updatedAt` records the new installation. Removing the registration and installing again starts a new creation identity
- missing published targets fail before any simulation env is created
- a selected runtime that changes before the clone switches fails the `prepare target` check instead of validating a different release; rerun the simulation to select the target again
- `--scenario all` runs built-in current, clean minimum, and Telegram-configured env shapes as separate simulation clones
- target runtime installation, checkpoint preflight, and runtime recovery preparation run while the current managed gateway remains available; preparation failure leaves it unchanged
- a running managed gateway is stopped only for checkpoint capture, runtime publication, environment mutation, and finalization
- new checkpoints capture the complete environment root; APFS clone support is used when available and a metadata-preserving full copy is the fallback
- when an env moves to a new runtime, OCM runs OpenClaw's update finalization path cold before service restart
- OCM rejects structured finalizer errors and plugin capability-consent blockers before publishing the replacement binding, even if the finalizer exits successfully; ordinary nonblocking plugin warnings and completion-cache failures remain allowed
- finalization can change environment state, so a rejected finalizer still restores the pre-upgrade snapshot and restarts the previous managed gateway unless `--no-rollback` is set; it does not start the rejected replacement
- if service reconciliation fails, OCM restores the snapshot and previous runtime unless `--no-rollback` is set
- pinned runtimes stay pinned unless you pass `--version`, `--channel`, or `--runtime`
- local-command environments are reported clearly instead of being changed behind your back
- dev environments registered with `ocm dev` are reported as `dev-checkout` and left on their checkout; update that checkout outside OCM
- under `upgrade --all`, environments without a runtime or launcher binding are reported as `unbound` and skipped; `ocm upgrade <env>` still refuses them until you bind one

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

Foreground `ocm env run` and `ocm @<env>` commands retain their selected runtime
and its managed backing files until the command finishes, including a one-off
`--runtime` override. Direct removal waits for that active use to finish, then
checks the persistent bindings again. Concurrent commands may share the same
runtime; unrelated runtime operations remain available.

Runtime registrations and managed directories share a namespace: the registration for `old` occupies the install directory of `old.json`.
OCM refuses an installation that would replace another runtime's registration or payload, including with `--force`; choose another name when these paths conflict.
Names ending in `.json` remain supported when their destinations are available, and `--force` still replaces an unbound runtime of the same name.

When a runtime moves to another release through `ocm runtime update`, `ocm upgrade` or a replacing official `ocm runtime install`, a description OCM recorded (`Official OpenClaw release <version>` for any version, or the release manifest's description of the runtime's current release) is replaced by the new release's, while a description set with `--description` is kept. A manifest description is recognized only while the manifest still lists the runtime's current release with the same description; otherwise the stored description is kept.

Runtime installs unpack into a hidden staging directory and publish it only when they succeed.
If OCM is interrupted or killed during an install, a later install that prepares a new runtime, of any name, removes the leftover staging directory.
A leftover is kept while another operation holds that runtime's lock and is removed by a later install after the lock is released; removal is best effort, like the cleanup of an ordinary failed install.

### Package runtime Node.js selection

Official package installation reads `engines.node` from the integrity-verified archive before invoking npm.
Ordinary runtime execution reads the installed package manifest and uses a compatible host Node.js when available, otherwise a compatible private toolchain.
On Windows, OCM invokes that Node.js executable with the installed `openclaw.mjs` entrypoint followed by the original arguments. Registered commands and other custom executable runtimes keep their existing direct execution.
OCM validates cached executables with `node --version` and reuses only its selected private Node.js patch.
Private toolchain preparation for release flows, including reuse of an already-installed cache, also makes a best-effort attempt to remove hidden `.node-toolchain-*` download and extraction stages left by exited OCM processes.
Stages are kept while their recorded process is still running or cannot be inspected, including when its PID has been reused; a later preparation retries cleanup.
Completed shared toolchain caches, unrelated files and directory links are preserved.
Run `ocm runtime install --channel stable` while online to refresh an older private cache, including when the OpenClaw package is already installed.
Refreshing a bound runtime also synchronizes its managed service. If the selected Node.js executable changes, the gateway restarts asynchronously. Use a maintenance window and run `ocm service start <env>` to wait for normal gateway readiness before checking health.
Once refreshed, the selected private runtime remains usable offline.
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

On Unix, recipes beginning with the `exec`, `eval`, `command`, or `.` (dot-source) shell builtin use the shell for managed services as well as `env run`, with or without quotes in the recipe. In particular, `command /path/to/openclaw` does not require an external executable named `command`, `eval` retains its shell evaluation semantics, and `. /path/to/wrapper.sh` sources the wrapper instead of trying to execute `.` as a program.
Recipes beginning with `source` or `builtin` (including `builtin source`) also retain shell execution, including when the wrapper path is unquoted. These forms require a shell that supports them; OCM does not select a different shell to make them available. Foreground `env run` uses `sh` from PATH, while managed services use `/bin/sh`.
Both run as login shells, so tool setup in the profile that `sh -l` reads (`/etc/profile` and `~/.profile`), such as a `PATH` entry for a Node version manager, still applies to the launcher command. After the profile runs, OCM restores the environment's own values for every `OPENCLAW_*` variable and for `OCM_ACTIVE_ENV`/`OCM_ACTIVE_ENV_ROOT`, the same variables it controls for runtime-bound environments and `env exec`. A profile that exports OpenClaw paths or activates another environment therefore cannot redirect a launcher command; the diagnostics variables and `OPENCLAW_INCLUDE_ROOTS` that OCM forwards from your shell are left as the profile sets them.
Sourcing support and argument forwarding depend on those shells. For example, macOS `/bin/sh` supports `source` and `builtin source` and forwards arguments into the sourced script, while other `sh` implementations may not. For portable argument forwarding, use an executable wrapper that invokes OpenClaw with `"$@"` and register the wrapper path directly.
Plain executable recipes still run directly under the supervisor.

### The simple rule

- published OpenClaw release: use `release` and `runtime`
- local checkout as source command: use `launcher`
- local checkout as built package: use `runtime build-local`; add
  `--include-source-extensions` only when the managed artifact should retain
  source-checkout extensions omitted from the release package
- day-to-day work: use `env`

Runtime and launcher names use letters, numbers, `.`, `_` and `-`. The name
`none`, in any letter case, is reserved: `ocm env set-runtime <env> none` and
`ocm env set-launcher <env> none` clear the binding and report it as cleared.

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

`ocm @<env> --`, `ocm env run` and `ocm env exec` exit with the command's exit
code. On Unix, when a signal ends the command, OCM exits with 128 plus the
signal number, as a shell does (for example 137 for `SIGKILL`), and names the
signal on stderr unless it was `SIGINT` or `SIGPIPE`.

OCM env execution sets OpenClaw's env-scoped paths for you:
`OPENCLAW_HOME`, `OPENCLAW_STATE_DIR`, and `OPENCLAW_CONFIG_PATH`.
It also sets `OPENCLAW_SERVICE_REPAIR_POLICY=external`, which tells OpenClaw
that service registration and repair are managed outside OpenClaw in this
context. Commands like `openclaw doctor --fix` can still repair normal env
state, but background service lifecycle should be handled with `ocm service`.

On Unix, `ocm @<env> --`, `ocm env run` and `ocm env exec` start the command
in its own process group and return only after it has exited. A SIGTERM,
SIGINT, SIGQUIT or SIGHUP sent to the OCM process is passed on to that group,
including processes started by a launcher's shell, so `kill <ocm pid>` also
stops a foreground Gateway. In a terminal, the command's group holds the
foreground while it runs, so Ctrl-C reaches it once, directly, and Ctrl-Z
suspends OCM along with it. After passing a signal on, or when the command dies
from Ctrl-C or a hangup, OCM also waits for the rest of its process group. If
the job started in the background and was resumed with `fg`, the same wait
applies after terminal interruption. If that SIGTERM, SIGINT or SIGHUP is what
ended the command, OCM then ends by the same signal instead of exiting with
128 plus its number, so a calling shell or
supervisor sees the interruption. SIGKILL cannot be passed on: sent to OCM or
its process group, it stops OCM alone.

When OCM shares its terminal job with other processes, for example in
`ocm env exec mira -- openclaw logs | less` or under a script that runs it in
the foreground, the command stays in that job instead, so the pager or script
keeps the terminal. Terminal keys then reach every process in the job, and the
four signals above, sent to OCM, are passed on to the command itself, which may
therefore see a terminal key twice. In that case processes started by a
launcher's shell are only signalled if the shell replaces itself with the
command; `dash`, the usual Linux `sh`, does not.

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
ocm service status
ocm service status mira
```

Use `service install` when you want a background service for an environment that was created with `--no-service`, or when you want to bring an older env under background management later. Install enables the environment without starting it; running it again for an environment whose service is already running keeps that Gateway running.

`service status` without a name lists every supervised environment.

On Linux, OCM writes its user-service unit under `$XDG_CONFIG_HOME/systemd/user`
when `XDG_CONFIG_HOME` is an absolute path. An unset, empty, or relative value
uses `$HOME/.config/systemd/user`, matching systemd's user-unit lookup.
Run OCM with the same XDG configuration as the user manager; changing a shell's
environment does not reconfigure an already-running manager. Service inspection,
start, stop, and uninstall use the same directory. Supervised children inherit
the spawning daemon's XDG configuration so their OCM commands find that same
service. macOS continues to use `$HOME/Library/LaunchAgents`.

Service readiness requests `/health` on the local Gateway port using the
configured transport. A `gateway.bind: "custom"` with an IPv4
`gateway.customBindHost` assigned to this machine uses that address, including for pinned runtimes that
do not add a second listener on `127.0.0.1`. A `gateway.bind: "tailnet"` uses the
native Gateway's first IPv4 address in `100.64.0.0/10` on an active external
local interface. It also retains the loopback probe for the native fallback and
newer Gateways' second listener, including when an interface appears after a
Gateway started on loopback. Both probes share one time budget. Other bindings and the wildcard `0.0.0.0`
retain the `127.0.0.1` probe. Custom loopback addresses are local; other selected addresses
are checked against OS interface assignments before connecting. Nonlocal
addresses or failed interface inspection keep the Gateway unready. Readiness
never resolves hostnames or connects to a remote address. Bind settings use the same supervised launch context, authorized
includes and variable resolution as certificate settings below.
Readiness resolves only the relevant Gateway settings, so
unrelated shared configuration includes do not block health checks. With
`gateway.tls.enabled: true`, OCM verifies the peer
against the environment's configured public `gateway.tls.certPath`, or the
Gateway's default `.openclaw/gateway/tls/gateway-cert.pem`, and verifies its TLS
handshake. This supports the Gateway's generated self-signed certificates as
well as explicitly configured certificates. Relative certificate paths follow
the Gateway's launch directory; `~` follows its isolated OpenClaw home. OCM
never generates certificates or reads the server private key. Once TLS is selected
for a readiness wait, OCM never switches to plaintext after configuration,
certificate, handshake, or health-check failures. A successful native lookup can
complete an initially partial observation, respecting later include overrides;
failed or unavailable resolution retains any partial TLS evidence. For templated certificate paths in dev and
installed OpenClaw package bindings, it makes one private, bounded native
`config get gateway --json` call so OpenClaw owns `.env` resolution. The same
native lookup resolves needed includes that its own include-root policy permits.
The result is reused while observing that child; a supervised replacement gets
a fresh lookup within the same readiness deadline, retaining any observed TLS requirement.
Generic launchers and registered executables are not sent configuration commands;
their certificate paths use literals, supervised-process variables, or `config.env` values.
For a generic launcher whose transport cannot be observed within the admitted
configuration roots, OCM retains the ordinary HTTP health check only when it has
not observed TLS enabled. TLS enabled solely inside a wrapper's private
configuration remains unsupported and can receive that legacy HTTP probe.
Unknown certificate settings after TLS is observed, missing or mismatched
certificates, and failed native configuration lookups keep the Gateway unready
within the normal readiness timeout. HTTP environments retain the ordinary
HTTP health check.

The `URL` in `service status <env>`, and the `url=`/`URL` output of
`dev <env> --service` and of a service restored after `dev` or source watch, use
`https://` when the Gateway's TLS setting is enabled, or was observed enabled
before an inaccessible include, and `http://` otherwise. The setting is read in
the service's launch context: the plan the daemon launched for a running
Gateway, otherwise its current binding's plan. These outputs only read local
configuration: they never run the native config lookup, so a TLS setting visible
only to that lookup is not reflected. JSON and raw `service status` output
contain no URL.

On Unix, OCM waits for Gateway PID publication before moving a matching caller
out of the Gateway process group during daemon refresh, snapshot, or upgrade
maintenance. This protects the caller from the supervisor's signals to that
group. A systemd unit stop or restart can still terminate the caller through its
service cgroup.

### Supervisor bootstrap logs on a separate filesystem

By default, the supervisor's own stdout/stderr logs live under
`$OCM_HOME/supervisor/logs`. If the OS service manager cannot open logs there
(for example, on an affected macOS external volume), select a dedicated,
absolute directory on an accessible filesystem when installing the service:

```bash
OCM_DAEMON_LOG_DIR="$HOME/Library/Logs/OCM" ocm service install mira
```

OCM saves the selected directory in
`$OCM_HOME/supervisor/daemon-log-config.json`. Later status, lifecycle and
upgrade commands reuse it without requiring the variable. The service definition
contains the resolved paths, so startup does not depend on a terminal's
configuration. Each store remembers its own choice; the OS user still has one
shared OCM service registration, and the existing store-ownership checks apply.

For an already-installed service, apply or change the selection during a
maintenance window:

```bash
OCM_DAEMON_LOG_DIR="$HOME/Library/Logs/OCM" \
  ocm service refresh-daemon --acknowledge-gateway-restarts
```

The variable selects a destination for service definition generation. Status
commands report the saved choice without changing it. OCM retains the selection
if service activation fails after the definition was written, allowing a later
retry without the variable; a definition-write failure restores the previous
selection. To return to the default, refresh with the store's default log path;
OCM removes the saved override:

```bash
OCM_DAEMON_LOG_DIR="${OCM_HOME:-$HOME/.ocm}/supervisor/logs" \
  ocm service refresh-daemon --acknowledge-gateway-restarts
```

Only `daemon.stdout.log` and `daemon.stderr.log` use this directory. Environment
data, runtimes, workspaces, gateway logs and the supervisor working directory
stay under their existing paths. Existing log files are not moved or deleted.
OCM creates the log directory using its normal private-directory permissions;
choose a dedicated directory, not a shared one. Empty and relative paths fail.

This does not grant macOS privacy permissions or make an unmounted store
available. The store and executable must still be accessible at startup.

### Read logs

```bash
ocm logs mira --tail 50
ocm logs mira --stream error
ocm logs mira --follow
```

Log output can be piped to a reader such as `head -n 1`. When that reader closes
its input, snapshots and followers terminate quietly with success, including
in shells using `set -o pipefail`. A follower detects closure when it next writes
output. Log read failures and other output errors still fail the command.

`logs` reads the newer of the env's own Gateway log and the background service
child log. A single-stream follow such as `ocm logs mira --stream error --follow`
started before either file exists waits for both and follows whichever appears
first, so it shows the service output after a later `ocm service start mira`.
Once attached, it stays on that file like `tail -f`.

For an environment named `daemon` (including case variants), service-child output
uses `.daemon.stdout.log` and `.daemon.stderr.log` in the supervisor log directory
(with the environment's original case). The supervisor's own `daemon.*.log`
diagnostics remain separate. A child-output alias must point to an existing,
separately owned file; an alias with a missing target is refused at child startup,
including while diagnostic files are being rotated. New child output is also
refused while a diagnostic alias has a missing target. Restore separately owned
targets or remove the unsafe aliases before retrying. At child startup, OCM also
refuses output into a file still held open as the supervisor's own stdout or
stderr, even if its diagnostic pathname has been rotated or replaced.

The log reader omits the old `daemon.stdout.log` and `daemon.stderr.log` child
destinations, which can contain output from both owners, and excludes child files
that match the current diagnostic files in the default or configured diagnostic
directory. These identity checks apply to snapshots and follow, including
retained file handles after rotation. Existing followers pick up diagnostic-directory
changes made by `service refresh-daemon` on their next poll. Daemon-named service
followers read the small saved directory configuration once per 250 ms poll,
shared across both streams; ordinary names and attached Gateway logs need no
extra configuration reads. If the configuration becomes unreadable, service
output pauses while attached Gateway output continues. OCM preserves the excluded
files and keeps independently owned child history readable.

OCM does not record which files received diagnostics in the past. If rotation or
removal leaves a diagnostic file reachable only through a child-output alias,
with no matching current diagnostic file, a fresh reader cannot distinguish it
from legitimate child history. That alias can still expose old diagnostics;
retrospective attribution of such unrecorded history is unsupported. The
supervisor's startup checks do not give the reader access to its live output
handles or establish historical ownership.

Existing running services are not restarted by these protections. Output from an
older running daemon remains in its old files. Explicitly refresh that daemon
from the updated OCM installation during a maintenance window to enable separate
child output. The environment's own Gateway logs remain available as before.

By default, `ocm logs` merges stdout and stderr in timestamp order and shows the
last 50 lines. A line without a timestamp stays after the timestamped line
before it. Untimestamped output at the end of a log, such as the error a Gateway
prints when it fails to start, is placed at the time that file was last written,
so the newest failure remains at the end of the merged view.

While following, complete undated lines are shown as they arrive when no
timestamped record is open, even if the writer stays busy. An unfinished line
waits for more bytes or a quiet poll. Timestamped records keep their continuation
lines together until the next timestamp or a quiet poll.

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
Once daemon shutdown is requested, startup batching stops admitting capability
probes and Gateways. A capability probe already in progress may finish within its
existing timeout before the daemon proceeds to owned-child cleanup; its result
does not start a new Gateway. This does not change saved service-running intent.

Starting a service requires a successful manager observation or confirmed absence of the OCM daemon.
If the manager cannot be queried or reports an unknown process state, OCM refuses activation and preserves the existing daemon and other environment services.
Retry after restoring manager access; `ocm service status` remains available for best-effort inspection.

Saved dev service plans must still match the env's current binding and recorded
worktree. If OCM refuses a stale plan, use `ocm service restart <env>` to refresh it.
Pending starts recheck the current effective Gateway port after capability
probing. Removing and recreating an environment with a different selected port
cannot start the removed environment's old-port plan; normal reconciliation
starts the replacement plan. Equivalent recreation does not force a restart.
Pending runtime and launcher starts also recheck the selected binding after
capability probing. Clearing a binding with `ocm env set-runtime <env> none` or
`ocm env set-launcher <env> none`, or replacing a launcher, prevents a pending
probe from starting the removed binding. This preserves service-running
intent; normal reconciliation applies the updated plan and any remaining binding.
Changing a binding does not instantly terminate an already-running Gateway.
Launcher admission also checks the current command and working directory, so
clearing, removing, and re-registering a launcher under the same name cannot start
its retired command. Re-registering the same command and working directory remains
compatible; description and timestamp changes alone do not invalidate a pending start.

Normal restart is gateway-aware while the Gateway is running and
`ocm service status mira` reports restart handoff `protocol v1`: OpenClaw records
eligible active sessions and subagents,
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
a changed launch definition, or a new daemon run can start it again. Ordinary
restart starts a settled inactive Gateway without requiring `--force`, even when
its runtime record retains restart-handoff capability. OCM verifies the current
daemon observation before starting it; stale or unresolved ownership does not
qualify as a stopped Gateway. Stopping the service removes its inactive runtime
record so snapshot maintenance can verify that the environment is quiescent.

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
Each fleet child retains only its own environment's lock. If distinct registered
names share that physical lock on a case-insensitive filesystem, it excludes
mutations of both names until the child releases it.
This is not crash recovery or proof that all descendants stopped: a program can close inherited descriptors before spawning further work, and Windows retains its existing locking behavior.

If a later mutation keeps waiting, use `lsof "${OCM_HOME:-$HOME/.ocm}/locks/environments/mira.lock"` to inspect processes with that environment's lock file open.
Distinguish waiting OCM commands from surviving upgrade children, and let active update work finish.
Stop an unintended holder only after verifying that interrupting its work is safe, then retry the mutation.
Do not delete or replace the lock file to bypass a holder: a new file can admit another writer while the original holder is still running.

### Upgrade a selected batch

For an explicit fleet transition, `ocm upgrade batch --runtime <runtime> --envs
<first>,<second> --parallel 2 --accept-fleet-outage` captures every selected
environment's checkpoint before starting upgrades. `--dry-run` previews the
selection without the outage acknowledgement. Distinct names such as `Demo`
and `demo` retain their exact identities and must have separate environment
roots. When their existing lock files identify the same physical file, the
batch acquires that lock once and processes those environments serially in
both checkpoint and upgrade phases. Independent lock groups still run up to
the requested parallelism. Existing lock paths and exclusion with other OCM
processes are preserved.

### Upgrade every environment that can be updated safely

```bash
ocm upgrade --all
```

This updates channel-tracked environments and restarts their running services when needed.
Pinned, manual-runtime, local-command, `ocm dev` and unbound environments are reported as skipped and left unchanged.

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

An explicit OCM `--port` takes precedence over `gateway.port` in the OpenClaw
configuration. Otherwise, a valid configured port overrides the automatically
assigned port. OCM reads the effective JSON/JSON5 configuration, including
`$include` files within the config directory, for both inspection and command
execution. Missing or invalid configuration falls back to the saved automatic
port. An active foreground dev session continues to use its captured endpoint
until stopped, even if the configuration changes.

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

Registered sources retain their required Git object stores, including relative and
transitive `objects/info/alternates` references from shared clones, linked
`objects/pack` directories and pack files in those stores, and their access aliases,
as well as those of initialized submodules declared in `.gitmodules`, recursively.
When the working `.gitmodules` file is absent, OCM uses Git’s index or HEAD copy.
Removing an environment that contains this storage or a directory required to
resolve its stored alternate path is refused before deletion. This includes
ordinary directories before `..`, even when the object store is outside the
environment. Shared ancestor directories are read dependencies, not ownership of
their contents. An independent clone with its own objects does not keep its former donor reserved.
Missing borrowed object paths and their existing access-route directories stay
reserved, while unreadable or excessive Git
dependency metadata requires restoring that metadata before cleanup can proceed.

This includes loose-object fanout directory symlinks and individual object-file
symlinks, even when introduced after registration. OCM inspects Git's native
fanout paths using filesystem name equivalence and streams their entries for
aliases, for both SHA-1 and SHA-256 stores. It conservatively protects all aliases
within those directories, including missing targets for borrowed-source cleanup.
Ordinary loose files and independent hardlinks do not add dependency records or
change source identity when Git writes new objects. Uninspectable alias targets
can prevent admission; the existing dependency bound applies to aliases, not the
number of ordinary loose objects.

Repository configuration is part of the same protected footprint: the common
`config`, enabled `config.worktree`, and active included configuration files,
including user/system configuration, their access symlinks, and traversed directories.
A directory before `..` in an active include path remains required even when the
configuration file is physically outside it. Git selects conditional includes in the source
repository context; inactive include targets do not reserve another environment.
Inspection reads configuration names and origins without running hooks or configured
commands. Unreadable or invalid configuration refuses the operation before deletion.
Sources may share read-only settings without blocking independent upgrades; an
upgrade still refuses when its mutable checkout or Git storage overlaps another
source's required configuration.

Inspection time grows with loose entries and referenced stores. Complete scan
results are shared within one identity observation, but lock, preparation,
publication and destructive-operation rechecks observe the filesystem again.
Large stores and filesystems that require extra entry-type lookups can therefore
make those commands slower; there is no persistent cache or object-count limit.
Storage preservation does not imply universal Git maintenance support for object
file symlinks: Git can read an object while reporting its symlink as garbage in
`count-objects`, and local cloning has separate restrictions.

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
Clone, migration and adoption copy regular files, directories and symlinks. Unix sockets, FIFOs and device files in the copied home or workspaces hold no data and are left out, as in environment export; the source keeps them, and their owning processes recreate them when needed.
Clone retains shared managed skills under `.openclaw/skills` and their global ClawHub tracking metadata under `.openclaw/.clawhub`.
Clone and upgrade simulation preserve supported internal OpenClaw `$include` graphs, including nested absolute directives and shared include arrays. Absolute directives select the copied files; relative directives and source files stay unchanged. Includes must remain inside the configuration directory. Ordinary clone retains its restrictions on include-owned agent workspaces and sandbox origins, while simulation retains its identity overlay.
Agent directories remain separate from workspaces; external locations and paths reached through symlinks are rejected by these copy operations.
Upgrade simulation rejects an identity-dependent agent directory supplied by an include shared with unrelated configuration fields; give those consumers separate include files before retrying.
Git repositories whose administrative directories are already contained in retained workspaces rebase owned core.worktree and include paths to the destination.
Copied configuration referenced by these repositories survives cleanup without retaining unrelated siblings or acquiring external files.
Git continues to select conditional includes, and authored predicates, repeated-entry order, comments and unrelated signing/remote settings remain intact.
Relative values keep Git's containing-file or Git-directory context; external references and inactive missing or invalid includes remain unchanged unless their path maps to copied content.
Config files retain their copied permissions; unsafe ownership, active invalid configuration, required writes into protected history, or configuration locks fail the clone before registration and roll back its target.
Linked-worktree registrations and other Git storage pointers retain their separate copy handling.

Clone, both archive import modes, `migrate`, and `adopt import` also relocate
source-owned paths inside accepted OpenClaw config includes, including nested
and shared fragments. Plugin load paths point at the copied payload; explicitly
external values and source files remain unchanged. Relative `$include`
directives and fragments needing no rewrite retain their authored contents.
Rewritten fragments retain their copied Unix file modes.
Migration and adoption relocate absolute internal `$include` directives, including
nested directives, before resolving the copied graph. Accepted directives select
the corresponding target copies, including when the source home is supplied
through a filesystem alias and absolute directives use that same alias. Relative
directives stay relative. Includes outside
the source home, including symlink escapes, remain rejected.
The existing restrictions on include-owned agent workspaces and sandbox origins
still apply.

Migration and adoption give the new environment its own Gateway port. A
`gateway.port` set in the copied `openclaw.json` or in one of its copied
`$include` files moves to the port OCM assigned, together with an MCP app
sandbox port that follows it. When a copied include that sets the port is also
included elsewhere in the config, the command fails and names the include
instead of creating an environment that shares a Gateway port.

Migration and adoption also relocate installed-plugin records authored under either
the supplied home spelling or its canonical path. This includes internal local
plugins when the home is supplied through an alias. Payloads must remain inside
the selected source home; external active paths and symlink escapes are rejected
and the provisional target is removed.
Plugin load paths, including supported include fragments, select the same copied
physical payload. A shared fragment that would require different plugin-load and
other values is rejected; separate those include consumers before migration.

Clone does not copy dev source bindings. The usual next step is:

```bash
ocm start rowan
```

Clone (including upgrade simulation), `migrate`, and `adopt import` use SQLite snapshots for recognized operational databases: shared state and quarantine, canonical agent stores, legacy `memory/*.sqlite` stores, and the default Workboard, Team Reports, Logbook, and active Matrix account state stores.
Agent database paths recorded in the source shared-state registry are also included when the existing copy traverses them. Canonical registered paths match an accepted aliased environment root, including agent directories outside `.openclaw` but inside the environment.
An accepted internal `state` directory alias receives the same coherent capture for its shared and quarantine databases, and its registry still selects agent databases. The alias must resolve inside the existing copy; links do not expand the copy footprint, and unrelated files beside those databases retain their ordinary copy behavior. Clone also retains those database files and the internal links and directory entries needed to reach them through runtime cleanup, without preserving unrelated unconfigured neighboring data.
Each database is a coherent committed snapshot; databases and other files are not captured in one atomic transaction.
The source stays running and its database contents are not rewritten.

Plain-home migration (`migrate` and `adopt import`) rebases retained SQLite session locations and database registry paths into the copied state directory and configured external workspace copies, including filesystem aliases.
The default agent's implicit `.openclaw/workspace` is also copied when it is an
absolute symlink to an external directory, even without a workspace setting or
agent entry. The imported configuration selects that environment-owned copy;
the source directory, configuration and link stay unchanged. Ordinary default
directories retain their existing location. Other implicit workspace aliases
remain subject to the existing safety checks and roll back on refusal.
Session locations follow the actual shared workspace projection, while in-home database registry paths stay in the fixed copied state directory.
When a configured workspace contains the original home, its workspace locations select the enclosing workspace copy; active runtime databases still use the new `.openclaw` state directory.
Recognized legacy session headers with an in-home `cwd` follow the same workspace projection; other legacy text and external references keep their existing relocation policy.
It maps resolved locations so internal links do not leave retained sessions pointing back at the source; references outside the copied roots and unresolved links remain unchanged.
Symlinks in the copied home keep their targets, except that an absolute link into the original home, spelled through the home or an alias of it (for example `logs` or `agents/<id>/sessions` linked to another directory in the same home), is recreated as a relative link to the same location in the copy, so the environment does not keep reading or writing the source home. The same applies to links in copied external workspaces, which point at the new state directory, while an enclosing workspace copy that contains the home keeps such links within that workspace copy. Links inside saved skill-library and Skill Workshop revisions, relative links and links outside the home are copied unchanged; a link into the home that cannot be mapped fails the migration and rolls back the partial import.
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
Native linked Git checkouts in retained workspaces receive the same structural
relocation during clone, `migrate`, and `adopt import`, without requiring an
OpenClaw worktree registry. Their repositories and required Git storage must all
be inside the copied footprint. Staging in the destination then uses its own
index; the source checkout and registrations remain unchanged. An incomplete or
external Git graph fails the copy and removes the provisional target.
Ordinary shared Git checkouts created with `git clone --shared` also relocate
their object-alternate paths during clone, `migrate`, and `adopt import`. The
donor object storage must be inside the copied footprint. The copied checkout
can then read its history after the original environment is moved or removed.
Native bare shared repositories (`git clone --bare --shared`) also relocate
object alternates whose donor storage is already inside the copied footprint.
Their copied history remains usable without the source when all required donors
are included. External or omitted bare donor paths retain their existing values;
these operations do not acquire additional donor storage.
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

Plain-home migration also keeps external project and archive paths as source
information when the preferred `installPath` resolves to a copied internal
payload. The source project or archive is neither copied nor modified. An
external active payload, or a missing preferred installation whose ownership
cannot be established, still makes migration fail and remove the partial target.

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
On case-insensitive storage, accepted case-equivalent environment names with distinct roots can share the job directory.
Latest status selects the exact environment's newest request even after a peer submits a later job.
The shared admission lock still excludes overlapping workers, and an ID already used by a peer is refused; use distinct request IDs.
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
directories. Restore replaces owned entries around them, including captured symlinks
whose targets are missing or independently preserved, and removes owned entries
added after capture. It leaves current
independent bytes, modes, timestamps, extended attributes, symlinks, additions,
and deletions in place. Shared ancestor directories stay in place, but their
metadata can change when owned siblings are replaced. Existing service-log and
socket rules and SQLite validation still apply to the captured state.

If a case-only rename still lets the saved path select the same directory entry,
scoped restore preserves that entry and its current spelling, including shared
ancestors. This follows the filesystem's actual lookup behavior; it does not
track independent directories moved to paths that the saved path no longer finds.

Captured read-only ancestor modes do not prevent scoped restore: OCM temporarily
allows owner writes only to staging parents while moving owned entries, then
restores their permissions. Current shared ancestors keep their permissions and
identity; they must permit replacing owned children. OCM does not make a current
read-only ancestor writable or change the saved checkpoint's modes.

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
replace another registered environment's root or links needed to reach it,
registered dev source, active temporary dev source, or required Git metadata,
before service quiescence or restore staging. Legacy nested environment roots
remain protected regardless of their protection flag, including missing roots.
A saved exclusion must preserve the entire other root and its access paths;
excluding only a directory beneath that root is insufficient. Disjoint registered
roots do not block restoration.
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

On Linux, readable files and traversable directories with read-only modes and
ordinary `user.*` extended attributes can be captured and restored together.
OCM preserves their final modes and attributes without making the live source
writable or removing its attributes.

Clean, closed WAL-mode SQLite databases can also be captured and restored from
read-only directories without creating sidecars there. SQLite integrity checks
remain required; databases with WAL or journal sidecars retain normal SQLite
validation so committed sidecar data is not ignored. A database left with a hot
rollback journal by an interrupted writer is validated, and captured by export
and clone, as the committed data SQLite recovers from that journal; recovery
runs on a private copy, so the live database and journal are not modified.

A symlinked environment root is resolved for capture so the checkpoint contains
independent bytes; links inside the root remain links. Restore replaces the
registered root entry with the captured directory. If that entry is a symlink,
its former backing directory is left untouched. Older tree checkpoints whose
artifact is itself a symlink are refused on restore: they did not retain
historical contents. Create a new snapshot to protect the current contents.

Checkpoint storage must be outside the environment root being captured. If a
custom root contains `$OCM_HOME/snapshots/<env>`, including through a path alias,
snapshot and upgrade checkpoint preparation refuse with `checkpoint storage
overlaps environment root` before copying or stopping a running gateway. This
preserves live data and existing snapshots without recursively copying the
preparation directory. Disjoint custom roots, including roots elsewhere under
`OCM_HOME`, remain supported.

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

Snapshot ownership uses the exact environment name. If names such as `Demo` and
`demo` share a snapshot directory on a case-insensitive filesystem, each list
shows only that owner's validated records and `--all` shows each record once.
Existing snapshots remain readable in place, including snapshots retained after
an environment is removed. Corrupt or misplaced metadata still fails listing.

Show:

```bash
ocm env snapshot show mira <snapshot>
```

Restore:

```bash
ocm env snapshot restore mira <snapshot>
```

Restore keeps the snapshot's Gateway port. When OCM assigned that port
automatically and another registered environment now holds its port family,
restore selects the next available port the way clone and import do, moves a
`gateway.port` set in the restored OpenClaw config with it, and reports the
change as a warning. A port chosen with `--port`, or set through config that
restore does not replace (a linked config file, an include, or an independent
path), is left unchanged, and upgrade rollback keeps the environment's own
pre-upgrade port.

Restore retains the displaced environment until supervisor synchronization and managed-service acceptance succeed.
If synchronization fails, OCM attempts to restore the pre-restore root and registry before returning the error. A quiesced service resumes only when its prior data was unchanged or recovered successfully, including recovery with cleanup or resynchronization warnings.
An error distinguishes a failed rollback from restored data that still needs supervisor resynchronization or cleanup.
Before compensating failed managed-service acceptance, OCM pauses the rejected Gateway and confirms it is quiescent. If quiescence cannot be confirmed, it leaves the active files in place, retains the displaced pre-restore data in the reported operation directory, and withholds rollback and automatic service resumption. After confirmed quiescence and successful compensation, any required service restart uses a fresh Gateway against the recovered files.

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

With `--all`, retention is applied separately to each exact environment name,
including names whose snapshot directories refer to the same physical directory.
Show, restore, and remove require the snapshot's exact owner name.

With `--older-than <days>`, an age outside the supported date range is rejected
before any snapshots are removed. Omit `--yes` to preview the selected snapshots.

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

Import unpacks the archive in a new private directory under
`$OCM_HOME/import-staging`, on the OCM home's filesystem rather than the system
temp directory (`TMPDIR` does not apply), and removes it when the import
finishes. A later import removes staging left behind by an interrupted one.

Export writes `<env>.ocm-env.tar` in the current directory unless `--output` is
specified. The archive can be inside the environment, including through a
directory alias: its own output is excluded from the archived contents. An
existing output path is refused, including a dangling symlink or a file created
by another writer while export is preparing the archive. Both export modes
publish without replacing that entry and clean up only their own staging file
if writing or publication fails.

Export omits `.openclaw/state` by default. Default export keeps shared managed
skills under `.openclaw/skills` and their global ClawHub tracking metadata under
`.openclaw/.clawhub`, as clone does. The importing OCM must retain these roots
too: OCM 0.3.6 and earlier releases remove them during default import.

Default export also omits the legacy file stores
`.openclaw/credentials`, `identity`, `devices`, `cron`, and `memory`, which
default import removes as clone does, so the archive does not carry
credentials or device identity that no default import restores. Git
repositories and object stores inside those stores, and the Git configuration
files there that archived repositories select, are still archived because
import retains them. When the environment root is itself a Git checkout,
import retains its whole state directory, so the stores are archived too.
Include conditions that depend on location, such as `gitdir:`, are evaluated
where the repository is when exporting, with the exporting user's own Git
configuration. Selecting these Git files runs `git`
when one of those stores and a Git repository both exist.

`--include-state` means "export this
environment as the same installation": it includes every file under the
environment root, including plugin payloads and dependencies, SQLite-backed and
file-based sessions, and unrelated or legacy data. Only non-durable process
files and rebuildable runtime debris are excluded, using the snapshot rules
for sockets, pids, locks and temporary directories, plus extension dependency
caches and workspace runtime debris. Each SQLite database anywhere under the
root is captured as a coherent committed snapshot, including uncheckpointed WAL
writes; raw database sidecars are omitted. Databases and other files are not
captured in one atomic transaction.

Both export modes leave out Unix sockets, FIFOs and device files, such as a Git
filesystem monitor socket in a workspace repository. They hold no data; the
source keeps them, and their owning processes recreate them when needed.

Default export accepts configured workspaces under an aliased environment root using either the accepted root spelling or its canonical path.
On Windows, ordinary and extended-length prefixes are equivalent representations of those same roots.
Equivalent spellings select the same owned content and omit each workspace's `.openclaw/tmp` runtime debris.
Other root spellings retain the default export refusal, even when another alias resolves inside the environment.
External workspaces, the environment root itself and workspace paths redirected by internal symlinks remain refused.

Within the selected content, ordinary export preserves symlinks, including links
named `*.sqlite`, without copying their targets. Regular `*.sqlite` files still
use coherent committed snapshots, including uncheckpointed WAL writes, and omit
raw database sidecars. If a source parent is replaced during acquisition, export
retries from its captured root or fails without publishing a partial archive; it
does not copy bytes through a replacement symlink.

New archives, with or without `--include-state`, retain the accepted source root and its captured canonical path in optional `env.canonicalSourceRoot` metadata.
Configuration relocation uses both recorded roots without requiring the original source or alias to exist; state-inclusive runtime-reference relocation uses the same identities.
Absolute internal config includes use those identities to select the archived files before import inventories configuration, including nested includes. Imported directives select the destination copies; relative directives remain relative. Includes outside the config directory, including symlink escapes, are still refused.
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
Owned file and directory links use relative targets in the archive. A link that
names another owned link keeps naming it when every step of its path stays
inside the environment and is exported; otherwise it points at the resolved
target. External and dangling links retain their targets without copying
external contents.
Import relocates selected Git repositories and linked worktrees to the destination before registering the environment.
This includes native bare repositories (`git init --bare`): source-local configuration includes resolve to their retained destination files in both archive modes.
Native bare shared repositories also relocate alternates to donor storage already
present in either archive mode. Default export does not expand its selection to
include otherwise omitted bare donor stores; import leaves those absent or
external donor entries unchanged.
For checkouts and managed worktrees, required Git storage must be present inside the copied archive; an incomplete graph or unresolved copied lock fails import and removes the incomplete target without modifying the archive or source.
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
applies these checks when a stopped borrower is started from another checkout;
`--force` never switches a running environment. One-shot execution and status
use the new binding immediately; the next explicit service start rebuilds its
launch plan, and admission rejects a saved plan that still names the old source.

### Inspect and repair

```bash
ocm env status mira
ocm env doctor mira
ocm env cleanup mira --yes
```

Cleanup clears runtime and launcher bindings only when their metadata is confirmed absent.
Unreadable or invalid metadata is reported as `broken` by `env doctor`, with the
underlying error; preview and apply preserve that binding. Restore access or repair
the metadata, then rerun doctor. Restoring valid metadata allows execution to
recover without rebinding. `--all` applies the same rule.

Config repair moves paths under another registered environment's root into this environment's root.
Without a registered owner, it maps only paths inside another `.openclaw` directory onto this environment's `.openclaw` directory and leaves other paths unchanged.
Paths in your own `~/.openclaw` are references to your existing OpenClaw home, so doctor does not report them as copied paths and config repair leaves them unchanged.
The default workspace setting is checked separately: when `agents.defaults.workspace` points at a `.openclaw/workspace` directory outside the environment, including `~/.openclaw/workspace`, doctor reports it and cleanup resets it to the environment's own workspace.

`env status`, `env doctor` and `runtime verify` report a runtime as broken when
its binary is missing, is not a regular file, or cannot be executed by the
current user. On macOS and Linux, restore the execute permission on the reported
path, for example with `chmod +x`, then rerun the check.

Runtime-state cleanup preserves configured agent directories inside the environment, including their authentication and state files beneath generated plugin dependencies.
It also retains shared managed skills under `.openclaw/skills` and their global ClawHub tracking metadata under `.openclaw/.clawhub`.
A runtime-state reset is planned only when a session transcript under `.openclaw/agents/<id>/sessions` records, in its header, a working directory under another environment's root or another `.openclaw` directory, as state copied from elsewhere does.
Paths that are only mentioned in transcript messages, logs, notes, authored skills or installation records do not trigger it.
State copied outside OCM without such a transcript is not detected, while a transcript left from a time when a workspace pointed into another environment still triggers a reset, so review the listed paths before applying.
A reset removes stale sessions, logs and unselected generated files beside those directories, along with the other runtime state outside the config, workspaces, agent directories, shared skills and managed plugin roots, such as copied credentials and device identity.
The preview lists each path the reset will remove, relative to the environment root, and the applied result lists what it removed; `--json` reports them as the action's `removals`.
If the reset or a later step fails, the error names the paths that were already removed and any path whose removal stopped partway, and an `--all` failure also names what earlier environments' resets removed.
Configuring an agent directory above `agents/<id>/sessions` preserves agent payload beside that subtree without hiding its stale sessions from cleanup.
If an agent directory is exactly `agents/<id>/sessions`, cleanup preview and apply refuse the conflicting ownership before changing any configuration or data.
Use a separate agent directory before retrying cleanup; `env doctor` reports the conflict.
External configured agent directories remain untouched and do not prevent this cleanup.
Cleanup preview and apply refuse a runtime reset that would delete a registered or active temporary dev source, or Git metadata it needs, before changing that environment's configuration, bindings or files.
The diagnostic identifies the dependent environment. Remove that dev environment, or stop an active temporary session with `ocm dev stop <env>`, before retrying.
Sources outside the reset plan, including sources fully retained by workspace preservation, do not block cleanup. Cleanup without a runtime reset keeps its existing behavior.
For legacy layouts created before overlapping roots were rejected, cleanup also refuses a reset that would delete another registered environment's root or a link needed to reach it.
This applies before any config, binding or file repair, even when the dependent environment is unprotected. Preserve any needed child data and remove that environment separately before retrying the containing environment's cleanup.
Registered roots outside the selected deletion entries, including roots retained inside a workspace, do not block the reset.
`--all` uses the same check for each environment; it does not roll back repairs already completed for earlier environments.
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

Remove environments that no `env use`, `env exec` or `env run` has touched within
a threshold (14 days by default); the command previews until `--yes` is given:

```bash
ocm env prune
ocm env prune --older-than 30 --yes
```

`--older-than` must be a positive number of days with a representable cutoff date.
An age outside the supported date range returns an error before any environment is removed.

Prune skips protected environments and environments whose background service is
set to run, such as those started with `ocm start`, because their Gateway is in
use without any of those commands. Run `ocm service stop <env>` first to make one
eligible again, or use `ocm env destroy` to remove it with its service.

The preview lists, under blocked, each old environment that removal would refuse
for a condition that already exists, with removal's reason: an active or unfinished
source watch, or cleanup that would affect another environment's registered dev
source or an active temporary dev source. `--yes` leaves those environments in
place and still removes the other candidates. Run `ocm dev stop <env>` or remove
the dependent dev environment, then prune again. JSON output lists them in
`blocked`, each with the environment summary and `reason`; `count` covers only
`candidates` or `removed`.

If apply stops on an error after removing some environments, for example because
an environment changed after the preview, it still prints the environments it
removed and exits 1. JSON output adds `"code": "partial_apply"` and the `error`.
Prune again after addressing the error; environments already removed are gone.

The service output the supervisor captured for a removed environment,
`$OCM_HOME/supervisor/logs/<env>.stdout.log` and `<env>.stderr.log`, stays in
place until an environment with the same name is created, cloned or imported.
OCM deletes those files when it registers the new environment, so the new
environment starts without the earlier one's service logs. Copy them before
reusing the name if you need them. OCM never deletes a file that belongs to
another registered environment or the supervisor daemon's own
`daemon.stdout.log` and `daemon.stderr.log`, for example when a case-insensitive
filesystem resolves the new name to one of those files. For daemon-named
environments, name reuse clears only their separate `.<env>.stdout.log` and
`.<env>.stderr.log` child output; the legacy `daemon.*.log` files remain preserved.

For legacy layouts created before overlapping roots were rejected, removal refuses
to delete another registered environment's root or a link needed to reach it,
including through an owned development worktree or its Git registration directory.
This applies to `env remove`, destroy preview and apply, and prune preview and apply.
Remove the child environment separately before removing its containing parent.
`--force` overrides only the selected environment's protection; it does not select
registered children for deletion. Disjoint environments remain independently removable.

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
On case-insensitive storage, accepted case-distinct environment names such as
`alpha` and `Alpha` keep separate dev session records. One environment's
session, active or finished, does not block commands for the other name or
removal and restore of other environments. The two names still share their
environment operation and Gateway admission locks, so those steps for one name
wait for the other.
`--force` temporarily takes over a running background service while backend watching is enabled and restores it on exit only if no later service policy request supersedes the takeover; it rejects `--no-watch` or `--service`.
Temporary takeovers resolve `--repo` to its canonical checkout before preparation.
Changing a selected path alias while the session runs does not redirect the
worker or environment commands. A repeat request through that alias is refused
if it now selects another checkout; stop the session before switching sources.
Equivalent aliases that still select the active checkout can reuse it.

Temporary takeover reuse checks the captured canonical checkout during preparation
and service restoration as well as while active. A request for another checkout
is refused; a matching request reports the current session's progress without
starting another process. Older non-dev sessions that have not recorded their
source cannot prove a match during startup or restoration: retry after that phase
finishes. This refusal leaves the existing session running, and `dev stop` and
recovery remain available. Active older sessions use their recorded source override.

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
loopback requests and leave recorded state unchanged. For a TLS Gateway running
with `--no-ui`, status uses an `https://` Gateway URL and pins the configured
public certificate before requesting `/health`. An unreadable or mismatched
certificate, a failed TLS handshake, or a non-200 response leaves
`gatewayHealthReady` false; status never retries a known TLS endpoint with HTTP.
The recorded foreground source, environment root and port take precedence over
later registration changes. Certificate paths are resolved in that source's
launch context, including the existing native lookup for templated paths.
Before executing that lookup, OCM validates the selected source checkout;
missing, replaced or mismatched sources stay inspectable with health not ready.
These are current configuration observations, not a saved copy of the running
Gateway's configuration or the launching terminal's inherited variables.

While a foreground session is active, `env status <env>` reports `openclawState`
from that session instead of the background service: `running` when the
session's recorded Gateway port accepts a loopback connection and `pending`
otherwise. It does not request `/health`, and its next step is `dev status <env>`
rather than a service command, because service install, start and restart are
refused until the session ends. Unresolved retained service ownership still
reports `unknown` and points to `service recover <env>` first.

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

## Shell activation

Initialize the helper for the shell evaluating the output, then select an
existing environment:

```bash
eval "$(ocm init bash)"
ocm_use mira
```

Use `ocm init zsh` or `ocm init sh` for those shells. In Fish, use
`ocm init fish | source`. With no shell argument, `ocm init` uses the `SHELL`
hint to select Fish, otherwise Zsh. Specify the current shell explicitly when
it differs from the login shell recorded in `SHELL`.

The generated `ocm_use` helper keeps that selection for later activations,
even when the inherited `SHELL` names a different shell. An explicit
`ocm_use mira --shell bash` still overrides the activation format. Direct
`ocm env use mira --shell bash` prints the script without evaluating it.
`--shell` accepts only `zsh`, `bash`, `sh` or `fish`; any other value is
rejected, as it is for `ocm init`.

The helper runs in the current shell but leaves the shell's other variables,
options and positional parameters unchanged, apart from setting and clearing
the activation variables.
It preserves the selected environment's exact paths, including spaces and
embedded newlines. If activation fails, the helper returns the command's failure
status without applying its output or changing the previous activation.

Direct `ocm env use mira` without `--shell` prints code for the shell that runs
it, so `eval "$(ocm env use mira)"` in Bash, Zsh or `sh` and
`ocm env use mira | source` in Fish activate correctly even when `SHELL` names a
different login shell. When the command is not run directly by a recognized
shell (Bash, Zsh, Fish, or a POSIX `sh` such as Dash, ash, ksh or mksh), it
falls back to the `SHELL` hint in the same way as bare `ocm init`. A wrapper
that runs `ocm env use` from one shell for evaluation in another should pass
`--shell` explicitly.

Activation preserves explicitly exported `OPENCLAW_INCLUDE_ROOTS`, so authorized
shared configuration remains available after selecting an environment. It does
not derive that authorization from configuration files. Unrelated stale
`OPENCLAW_*` environment controls are still cleared.

## Output modes

By default, `ocm` formats output for people in a terminal.

Use:

- `--json` for structured output
- `--raw` for plain output
- `--color auto|always|never` for explicit color control

Examples:

```bash
ocm service status --json
ocm env status mira --raw
ocm --color always runtime list
```

## Command map

### Top-level commands

- `ocm setup`
- `ocm dev`
- `ocm start`
- `ocm upgrade`
- `ocm doctor`
- `ocm self`
- `ocm env`
- `ocm migrate`
- `ocm adopt`
- `ocm release`
- `ocm runtime`
- `ocm launcher`
- `ocm logs`
- `ocm service`
- `ocm init`
- `ocm help`
- `ocm --version`

### `doctor`

- host

### `env`

- create, clone, list, show
- use, exec, run, resolve, status
- set-runtime, set-launcher, set-independent-paths
- doctor, cleanup, protect
- snapshot create, list, show, restore, remove, prune
- export, import
- artifact export
- remove, destroy, prune

### `dev`

- status
- rebind
- stop

### `adopt`

- import
- inspect
- plan

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

- status
- install
- start
- stop
- recover
- restart
- refresh-daemon
- uninstall

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
- removal refuses a root that contains another registered environment or its required links, even with `--force`
- preview-first behavior is used where it matters

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
