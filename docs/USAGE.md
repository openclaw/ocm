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

If you run `ocm setup` from inside an OpenClaw checkout, local mode can detect that and fill in sensible defaults.

### 5. Test a local checkout as a release-shaped runtime

```bash
ocm runtime build-local main-local --repo /path/to/openclaw --force
ocm start luna --runtime main-local
ocm upgrade luna --runtime main-local
```

Use this when you need to test what users will run after an OpenClaw release, but from a local checkout before publishing. OCM runs `npm pack` in the OpenClaw repo, which triggers OpenClaw's package prepack path instead of registering a source checkout command.

The installed runtime uses the same package layout as published OpenClaw runtimes:

```text
files/node_modules/openclaw/openclaw.mjs
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

## Service management

Use `service` when an environment should run in the background.

### Install and inspect

```bash
ocm service install mira
ocm service list
ocm service status mira
```

Use `service install` when you want a background service for an environment that was created with `--no-service`, or when you want to bring an older env under background management later.

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

### Clone an environment

```bash
ocm env clone mira rowan
```

Clone copies the workspace and env config into a new environment, gives the clone its own gateway port, rewrites env-scoped OpenClaw config paths under the new env root, keeps durable agent auth/settings for the same user, clears copied runtime residue like sessions, logs, and backups, and keeps the background service separate. The usual next step is:

```bash
ocm start rowan
```

### Snapshots

Snapshot create and restore stop a running OCM-managed gateway before copying or
replacing its root, then restore the recorded service policy. New snapshots are
verified whole-root checkpoints: they include secrets, browser state, SQLite
sidecars, modes, symlinks, plugin data, and unknown future paths. Restore stages
an exclusive candidate beside the live root and retains the displaced root
until service acceptance; failed acceptance restores the displaced root.

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
