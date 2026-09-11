# ocm

**Install, run, update, and manage OpenClaw — properly.**

OCM gives OpenClaw one coherent workflow across stable releases, local checkouts, supervised env gateways, upgrades, snapshots, and ongoing maintenance.

OpenClaw is easy to start once. It gets messier when you want more than one setup, need stable and local development side by side, or want confidence about what is actually running. `ocm` fixes that.

Once an environment exists, `ocm` can be your normal OpenClaw entrypoint:

```bash
ocm @mira -- tui
ocm @mira -- status
ocm @mira -- onboard
```

## What ocm manages

`ocm` keeps the moving parts separate:

- **envs** — isolated OpenClaw environments
- **runtimes** — installed and pinned OpenClaw releases
- **launchers** — named command recipes for local-dev or custom runs
- **services** — background OpenClaw processes tied to one environment

That split is what makes stable releases, local development, upgrades, and service management fit together cleanly.

## Why people use it

Use `ocm` when you want:

- one clean OpenClaw environment per project, task, or instance
- one command path for OpenClaw itself through `ocm @<env> -- <command>`
- published OpenClaw releases installed locally and updated safely
- local checkout workflows that feel just as normal as released builds
- one OCM background service that can supervise env gateways cleanly
- snapshots, export/import, and safer cleanup

## Install

Install the npm distribution, available starting with OCM v0.2.40:

```bash
npm install --global @openclaw/ocm
```

See [npm installation and updates](npm/README.md) for Node requirements,
project-local installs, npx, and daemon refresh behavior.

Install with Homebrew on macOS (Apple Silicon or Intel) or Linux x86_64:

```bash
brew install openclaw/tap/ocm
```

Upgrade Homebrew installations with `brew upgrade openclaw/tap/ocm`, not
`ocm self update`. Since v0.2.40, OCM refuses to overwrite a
Homebrew-managed executable. `ocm self update --check` remains available.

Install the latest release:

```bash
curl -fsSL https://github.com/openclaw/ocm/releases/latest/download/install.sh | bash
```

Install a specific release:

```bash
curl -fsSL https://github.com/openclaw/ocm/releases/download/v<ocm-version>/install.sh | bash -s -- --version v<ocm-version>
```

Update an installer-managed install:

```bash
ocm self update
ocm self update --check
```

Install from source:

```bash
cargo install --locked --path .
```

Source installs require Rust 1.88 or newer. Release installers verify the selected archive against the published `SHA256SUMS` before extraction.

Inside this repo, use the development wrapper:

```bash
./bin/ocm help
```

Published OpenClaw release flows in `ocm` prefer host Node.js `22.22.3+`,
`24.15.0+`, or `25.9.0+` and `npm`.
On supported platforms, `ocm` can manage a private copy for official release installs when those tools are missing.
Interactive release setup can also offer to install `git` for repo-aware coding workflows when it is missing.
Local checkout flows keep using whatever command and toolchain you choose.

## Start quickly

If you want the guided path:

```bash
ocm setup
```

If you already know what you want:

```bash
ocm start
```

`setup` walks you through the choices. `start` creates or reuses an environment, installs the latest stable OpenClaw release by default, writes the minimum local config needed to boot, and keeps it running in the background. Use `--onboard` when you want the interactive OpenClaw setup flow instead. If you do not pass a name, `ocm` generates one for you.

If you are developing OpenClaw itself, use the dev path:

```bash
ocm dev shaks
ocm dev shaks --root /tmp/shaks
ocm dev shaks --no-ui
ocm dev shaks --no-watch
ocm dev shaks --no-watch --no-ui
ocm dev shaks --force
ocm dev stop shaks
ocm dev shaks --repo /path/to/openclaw --watch --force
ocm dev shaks --service
ocm dev shaks --onboard
```

`dev` creates or reuses an isolated env, uses the selected OpenClaw checkout directly, bootstraps the minimum local config so the gateway can run immediately, and then starts the native Gateway watcher and live UI in the foreground by default. `--root` lets you choose the environment location. While a foreground dev session is active, `ocm @<env> -- ...`, `ocm env run <env> -- ...`, `ocm env resolve <env> -- ...`, service resolution, and `ocm env exec <env> -- openclaw ...` use the same source checkout and run `node <checkout>/openclaw.mjs` directly instead of rebuilding through the package script. `--service` installs and starts the dev env in the OCM background service instead of keeping the process in the current terminal. If a dev env is already running in the background, `--watch --force` temporarily takes it over for the watch session and restores the background service when watch exits. For an existing runtime or launcher env, `--repo <path> --watch --force` temporarily runs that source checkout against the env's real root, config, state, and port without changing its binding, tees foreground output to the env gateway logs, then restores a running background service when watch exits. `--onboard` runs local onboarding first and then starts the dev gateway. For a new env, `dev` uses the explicit `--repo` checkout or the checkout enclosing the current directory. Outside an OpenClaw checkout, pass `--repo`; `dev` does not select a neighboring or previously remembered repository. Existing dev envs continue to use their recorded source.

`--no-ui` runs the Gateway watcher alone. `--no-watch` keeps the live UI with a
Gateway that does not rebuild automatically. Combine both for a plain foreground
Gateway. `--watch` and `--ui` remain accepted; each conflicts with its negative
counterpart. `--force` requires backend watching and rejects `--no-watch` or
`--service`.

All foreground combinations support `ocm dev stop <env>` and reuse a matching
active session. `dev status` reports backend watching separately from session
ownership.

Creating or changing a dev binding is refused while an environment containing or
owning its source, worktree path, or required Git metadata is busy with another operation.
This also applies to local upgrade simulations; retry after that operation finishes.
Unchanged source bindings remain writable during restore and recovery.

New dev environments receive a private Gateway token in their initial config before
the environment is registered. The token remains stable through restarts and
source rebuilds, so paired clients can reconnect. Initialization never replaces an
existing config or its auth/SecretRefs. Repeating minimum setup leaves an unchanged
config untouched.

Foreground `dev` runs OpenClaw's native Vite server beside the Gateway by default;
use `--no-ui` to disable it.
OCM prints the session's UI address, then its initial native owner link after
both Vite and the Gateway Control UI document are ready. The native handoff keeps
the Gateway identity and credentials; only the browser document moves to Vite.
UI requires a local HTTP Gateway with Control UI enabled and installed UI dependencies.
For a templated `gateway.controlUi.basePath` such as `${UI_BASE}`, OCM reads the
effective path through the selected checkout's native config command. OpenClaw
resolves its environment, `.env` files, and config-provided variables; OCM keeps
that read private and leaves the authored config unchanged. An unresolved
placeholder is an error: supply its value or use a concrete path before retrying.
The resolved target stays captured for the session, including repeated starts
from terminals with different environment variables. Literal paths need no
additional config command.
`--service` uses the background workflow without foreground watching or UI and
rejects explicit `--watch` or `--ui`.

The controller owns Gateway, Vite and each dashboard helper. A component
exit stops its sibling; `dev stop` stops both and gives a running helper only the
remainder of its original 30-second budget. A late link is discarded, and uncertain
cleanup retains ownership. On Linux, macOS, and Windows, repeating the matching
command requests a fresh native owner link from that controller while keeping Gateway,
Vite and their address unchanged. One helper runs at a time; busy or unready
requests report a pending link. Requests have a 30-second deadline, and a caller
that exits or times out cannot redirect its eventual grant to another terminal.
Older OCM controllers retain address-only reuse and report fresh links as
unavailable until that dev session is restarted.
The UI address is retained across stop/start for the same environment. If that address is occupied
or reserved, startup fails until it is free. Cloned and imported environments
select their own addresses; restore keeps the current environment's address.
Stop the session before changing UI mode.

If you already have a plain `~/.openclaw` home you care about, use `ocm migrate <env>` instead of starting fresh. `setup` and `start` now point that out when they detect an existing plain OpenClaw home.

## Common paths

### Use the latest stable release

```bash
ocm start mira
ocm @mira -- tui
```

This is the shortest path for most people.

### Update OpenClaw later

```bash
ocm upgrade mira
ocm upgrade --all
ocm upgrade mira --dry-run
ocm upgrade history mira
ocm upgrade rollback mira --dry-run
ocm upgrade rollback mira
ocm upgrade simulate mira --to 2026.4.20
ocm upgrade simulate mira --to 2026.4.20 --scenario all
ocm upgrade simulate mira --to beta --scenario all
ocm upgrade simulate mira --to ./openclaw
```

Live upgrades and rollbacks require a completed foreground dev session. Request
shutdown of recorded ownership with `ocm dev stop <env>`. Older watches without
an unfinished ownership record must be stopped from their original dev terminal.
Unreadable or unverified ownership
requires verified operator recovery, including checking the watch processes and
service policy. OCM preserves environment state, runtime files, and upgrade
history while refusing those operations.

`upgrade` stages the target runtime, validates the checkpoint source, and
prepares runtime recovery while the current managed gateway remains available.
Preparation failures leave the source environment and service unchanged. OCM
stops a running gateway only for checkpoint capture, runtime publication,
environment mutation, and OpenClaw update finalization. If a running service
cannot be restarted or started after the change, OCM keeps the restored
checkpoint and previous runtime as the coherent rollback state. A running
managed service is considered recovered only
after its HTTP health endpoint responds and OpenClaw's gateway status proves the
gateway is reachable; otherwise the upgrade follows the normal rollback path.
By default, checkpoints preserve the complete environment root, including credentials,
browser profiles, plugin payloads, unknown future directories, modes, symlinks,
and SQLite sidecars. OCM verifies tree contents and SQLite integrity before
publishing the checkpoint. APFS uses copy-on-write clones when available;
other filesystems use a metadata-preserving full copy. For standard OpenClaw
node/gateway stdout and stderr logs under `.openclaw[-profile]/logs`, APFS
verification accepts the captured bytes only when they exactly match a prefix
of a stable clone of the same live file. Post-capture appends need not stop an
independently managed node. Logs are retained, not excluded; rewrites, unsafe
rotation, mode changes, SQLite (even at a log path), and all other state still
require strict verification. Full-copy checkpoints remain exact. Restore discards only
explicit process residue such as locks, sockets, PIDs, and temporary runtime
directories. Legacy tar snapshots remain readable.

If a workspace contains projects that OpenClaw does not migrate, declare their
directories as independent before upgrading:

```bash
ocm env set-independent-paths mira .openclaw/workspace/projects
```

Explicit declarations also support `.openclaw/worktrees` and non-hidden
development directories elsewhere in the environment home. Other hidden
home/state namespaces, including credentials and the state database, stay protected.

Upgrade checkpoints then omit those directories without reading their contents,
and rollback restores the surrounding owned state while leaving those directories
in place. The list is explicit and empty by default; names such as `node_modules`
never imply ownership. Whole workspaces and configuration files cannot be excluded.
Memory, identity documents, and legacy migration inputs remain covered unless
they belong to a directory explicitly declared independent. Declarations require
operator knowledge of migration ownership: OCM does not prevent OpenClaw itself
from writing to these directories. See [checkpoint scope](docs/USAGE.md#upgrade-checkpoint-scope)
for the contract and limitations. Separately requested `env snapshot create`
backups still include the entire environment.

On APFS, snapshot preparation clones the bulk tree while the gateway is running.
The final service pause reconciles changed or removed entries and checks changed
SQLite state before publishing the checkpoint. Failed or displaced preparation
trees are cleaned up after service restoration or upgrade completion, not before
restart. Other filesystems retain the fully verified copy path.

Checkpoints contain secrets and share the source filesystem. Keep `OCM_HOME`
private, allow enough space for checkpoint divergence plus a restore candidate,
and retain an off-disk backup for disaster recovery.
Snapshot removal validates that the stored environment, snapshot ID, and
artifact path match the named snapshot before deleting anything. It takes the
live metadata and checkpoint out of service together, then reports warnings if
linked upgrade recovery or staged artifact cleanup still needs attention.
When both OpenClaw versions are known, `upgrade` rejects an older target before
creating a snapshot, downloading the target, or changing runtime metadata.
Switching only the binary cannot reverse newer OpenClaw config or SQLite state
migrations; returning to an older release requires a checkpoint of its owned state captured
while that release and its state schema were active.
`ocm upgrade history <env>` lists completed upgrade transactions newest first,
including source and target bindings and versions, the pre-upgrade snapshot,
migration/finalization status, service state, and rollback outcome. History is
stored as atomic JSON metadata under `OCM_HOME`; it does not copy config
contents, command output, or credentials. A successful in-place update of a
managed runtime retains the previous runtime files beside its transaction
record. Removing or pruning the corresponding pre-upgrade snapshot removes
those retained files; switching to a different runtime does not duplicate the
source runtime.
`ocm upgrade rollback <env>` restores the newest completed upgrade or rollback
transition that has not already been reversed. Use `--transaction <id>` to
select a specific transaction and `--dry-run` to validate it without mutation.
Before creating a safety snapshot, rollback requires the current binding,
OpenClaw version, and service policy to match the selected transaction target;
it also verifies the recorded snapshot, source runtime or launcher, and any
same-name retained runtime recovery. A real rollback creates a `pre-rollback`
safety snapshot and a linked history transaction before it stops a managed
service or replaces runtime bytes. If restore or verification fails, OCM puts
the pre-rollback runtime and environment state back. Rolling back the linked
transaction safely reverses the rollback.
Dev environments retain their current source binding during restore.
Manual snapshot restore also keeps their current runtime/launcher binding;
upgrade rollback restores the recorded runtime/launcher instead.
Once an environment is bound to a runtime, direct `runtime update`,
`runtime install --force`, `runtime build-local --force`, and `runtime remove`
operations reject that runtime. Use `ocm upgrade <env>` so the environment gets
the snapshot, OpenClaw migration, rollback, and verification path, or clear the
binding first when intentionally managing an unused runtime.

For unreleased OpenClaw workspaces, `runtime build-local` follows the complete
transitive closure of private `workspace:*` packages. It rewrites nested
workspace specs only inside scratch archives before installation; the source
checkout remains unchanged.

For unreleased source builds with separately published, version-bound official
plugins, repeat `runtime build-local --companion <plugin-id>`. OCM packages each
selected plugin through OpenClaw's own package-local release contract, requires
exact host-version parity, stages it as a runtime-owned bundled plugin with
isolated dependencies, and records artifact and entrypoint hashes for runtime
verification.

`upgrade simulate` clones the source env, leaves the real env untouched, and
cleans temporary simulation envs and runtimes when the run finishes. For
published targets it first validates that the target exists, then runs OpenClaw's own
`update --dry-run --json` plan against the clone, switches the clone, and runs
update-mode doctor, plugin update dry-run, and gateway status checks. For local
repos it validates the checkout with dependency/build checks before running the
same post-update checks. Use `--scenario all` to test the current env config
plus built-in clean minimum and Telegram-configured env shapes. Use
`--keep-simulations` only when you need retained simulation envs and temporary
runtimes for debugging.

### Use a local checkout or dev build

```bash
ocm dev luna
ocm dev luna --root ~/scratch/luna
ocm dev luna --no-ui
ocm dev existing-env --repo ~/src/openclaw --watch --force
```

Use `ocm dev` when you want to run your checkout with separate environment state and a gateway port, or when you want to temporarily run source against an existing env in watch mode without rebinding it. While a foreground dev session is running, OCM's OpenClaw-running commands for that env resolve to that source checkout, so one-shot checks use its built `openclaw.mjs`. OCM refuses background service installation, start, and restart for that env until the foreground session exits; a running service taken over with `--watch --force` is restored by the watch session after its source processes stop. Foreground output is saved under that env's `.openclaw/logs/` directory, so `ocm logs <env>` remains useful while the foreground Gateway is running. If you are already inside an OpenClaw checkout, `ocm setup` can detect that and suggest a local path automatically.

On Unix, foreground Node commands wait until OCM records their process ownership before executing source or `NODE_OPTIONS` preload hooks. The gate preserves the command arguments, terminal input, and configured Node options after release.

Before starting a new foreground session or service preparation, OCM checks that a running background daemon uses
compatible ownership locks and that its runtime record identifies the current
process. A daemon whose compatibility cannot be verified must finish starting
or be refreshed before the session can claim ownership. During a maintenance
window, run `ocm service refresh-daemon --acknowledge-gateway-restarts` from the
updated OCM installation; this restarts managed gateways. Package version alone
does not establish compatibility. A confirmed stopped or unloaded daemon does
not require refresh, and existing session reuse and stop remain available.

New dev environments borrow the exact selected main checkout or registered linked
worktree, including its tracked edits and untracked files. OCM creates no Git
worktree for them. Their environment state stays outside the source and its Git
metadata. A borrowed environment resumes its recorded canonical source path;
an explicit `--repo` or another enclosing checkout cannot rebind it. Restore a
missing source, changed path alias, or invalid Git registration before retrying.

An existing OCM-owned dev env resumes its recorded worktree, preserving its uncommitted changes. If that worktree is missing or has been replaced by an unrelated checkout, `dev` reports an error instead of recreating it; restore the recorded checkout before retrying. An explicit `--repo` must still identify the env's original repository, including equivalent path aliases, and cannot rebind the env to another checkout.

Environment commands and Gateway resolution apply the same check when selecting
the dev source, including routing through an active source watch. Saved service
plans also validate the recorded checkout before starting it. Restore the
recorded checkout before retrying. Explicit runtime or launcher overrides and
status inspection remain available. A worktree validation failure leaves config unchanged.

Before starting source, `dev` checks the installed entry points for the tooling declared by that checkout. It reuses hoisted or isolated installs whose tooling resolves. New borrowed environments and OCM-owned dev worktrees can prepare missing tooling with `pnpm install --frozen-lockfile` and check it again. A resumed borrowed environment reports missing tooling so you can prepare its checkout explicitly; it does not reinstall dependencies. Source takeover of a runtime or launcher env only validates the borrowed checkout and fails before stopping its service if preparation is needed. OCM does not reinstall through linked dependency directories. A modules-directory environment override may select dependencies inside that checkout, including before OpenClaw creates its `node_modules` link. OCM inspects the configured tree without creating that link and rejects overrides that resolve outside the checkout. These are startup checks; OpenClaw still owns builds and validation of the running application.

Direct dev-source commands run with `NODE_DISABLE_COMPILE_CACHE=1` and without
`NODE_COMPILE_CACHE` before Node starts. This avoids OpenClaw's compile-cache
bootstrap wrapper, which can convert a child's termination signal to an ordinary
exit code. Other `NODE_OPTIONS` still pass through, and OpenClaw's native runners
continue to own rebuilds and auto-doctor.

Foreground dependency installation keeps build output and pnpm diagnostics readable while using lifecycle reports to check script completion. When run from a terminal, installation keeps interactive stdin while streaming that output. On Unix, interrupted or unfinished build scripts retain foreground ownership even if pnpm returns an ordinary error or an optional build lets it succeed. Completed installation errors remain retryable.

Repeating `ocm dev <env>` with the same effective backend watching/UI choices and source returns its status and existing gateway link without preparing dependencies, onboarding, or restarting processes. Mode/source/root/port mismatches and onboarding during a foreground session are errors. Startup and service restoration are reported as progress; an invocation during startup does not claim a source identity that has not yet been published. Temporary runtime or launcher takeovers retain the `--repo <path> --watch --force` form. Active status and routed source commands use the captured launch root and port even if config is edited; an explicit different port is refused. Watch sessions started by an older OCM without endpoint metadata need to be stopped from their original terminal and started again before they can be reused. Only the foreground invocation that acquires the lease prepares configuration.

Use `ocm dev status [env]` to inspect dev environments and temporary source watches on runtime or launcher environments. It reports the source path, gateway URL, and session ownership (`starting`, `active`, `restoring`, `inactive`, or `unknown`). Active foreground ownership can outlive the wrapper process and does not imply that the Gateway is ready. JSON reports backend watching as `sourceWatch.watching`, keeps `serviceRunning` separate from the saved `serviceDesiredRunning` policy and includes the watch PID, start time, and any inspection issue. `gatewayPortReachable` uses a 100 ms loopback TCP check; an open port does not establish Gateway identity or readiness. Status leaves watch metadata unchanged and omits lease tokens.

Named status and JSON/raw output also report `gatewayHealthReady` from `/health` and `ui.processRunning`/`ui.httpReady` for the recorded UI process and HTML document. Active sessions use their captured Gateway endpoint. UI readiness stays unknown when ownership cannot be verified; `ui.issue` explains the observation. The existing `uiUrl`/`ui_url` field retains the basic address of an active UI session.

Use `ocm dev stop <env>` from another terminal to cancel an owned plain or watched foreground session, including dependency preparation and onboarding. It waits for the source processes to stop and restores a background service previously taken over with `--watch --force`. The environment, configuration, source checkout, and dependencies remain available for the next run. `--json` reports `envName`, `stopped`, and `serviceRestored`; repeated stops after completion are harmless. Independently managed background services keep their existing stop commands.

`ocm dev <env> --service` owns dependency preparation and onboarding until their
processes finish. `ocm dev stop <env>` can cancel that preparation while keeping
an already-running managed Gateway and its launch plan intact. Sibling service
updates defer changes and restart requests for that Gateway until preparation
ends; an explicit service stop or uninstall still takes effect. Successful
preparation rechecks the environment, source binding, and service policy before
starting the service. Repeating `--service` with an unchanged binding avoids an
install/stop/start cycle.

For new Unix foreground generations, output readers must finish before OCM clears child ownership. A read failure, missing output completion, raw termination signal, or lost controller retains an unfinished session. Stopping a matching process group alone cannot prove detached workers stopped; subsequent stop, watch, service-start, and destroy requests preserve that uncertainty. Ordinary acknowledged errors remain retryable after output completes. Onboarding keeps real terminal output: unsuccessful or cancelled onboarding without observed output remains unfinished, while successful interactive onboarding still works. Released legacy watch records retain their existing recovery rules, and Windows keeps its process-job cleanup proof. Unknown or pending ownership still requires operator verification. Use an updated OCM CLI and refresh an incompatible running daemon before starting a new foreground generation; updating files does not refresh an old process.

After independently verifying that **all source processes, including detached workers, have stopped**, recover a retained cleanup failure with `ocm dev stop <env> --acknowledge-stopped-processes`. OCM still refuses a running controller, recorded child or process group, a held lease, unpublished child ownership, or changed environment/process scope. Recovery releases only the matching failed session; it preserves source, config, and current service policy without signaling processes or restarting a background service. You can then retry dev or remove the environment normally. Start a background service separately if needed. An empty process group by itself is not enough to make this acknowledgement.

`ocm env destroy <env> --yes` stops the recorded foreground generation and verifies shutdown before removing the environment. It rechecks the binding after stopping and preserves state if another owner or binding appears. While a watch is running, previews defer the changing process-tree inspection until after shutdown (`processInspectionDeferred` in JSON). `env remove` and `env prune` refuse active or unfinished watches; stop those sessions first. A guarded destroy with `--if-state-token` also requires `dev stop` followed by a fresh preview, so its original state guarantee remains intact. Completed watch records are removed with the env; synchronization lock files remain reusable.

New environment roots must be separate: a root cannot equal, contain, or sit
inside another registered environment root, including through path aliases.
Creation, cloning, import, and other commands that create an environment reject
overlap before writing environment state, regardless of protection flags.
Default sibling roots and disjoint custom roots remain valid.

These operations also reject roots that overlap registered dev sources,
including source and destination aliases. Missing borrowed paths
remain reserved until their binding is removed. Removing a borrowed environment
preserves its source, dependencies, generated output, and unrelated source workers.
Restore and rollback also preserve borrowed source and known Git metadata;
checkpoint exclusions must cover the affected paths, including missing source
reservations.

When a managed daemon is running, it must support new or changed borrowed
bindings before registration. Older OCM readers refuse these records. Refresh an
incompatible daemon with the command above; unchanged bindings and confirmed
stopped or unloaded managers remain usable.

Removal, pruning, destroy, and simulation cleanup preserve other registered dev sources and the Git metadata they need. Remove dependent dev environments before deleting their containing environment or worktree.

### Try beta or pin a specific release

```bash
ocm start rowan --channel beta
ocm start ember --version 2026.3.24
```

### Inspect an existing plain OpenClaw home before migrating it

```bash
ocm migrate mira
ocm migrate mira /path/to/.openclaw
ocm adopt inspect
ocm adopt plan --name mira
```

`migrate` is the simple front door for existing OpenClaw users. It imports a plain OpenClaw home into a managed env in one step.

`migrate` preserves config, auth, sessions, logs, and other durable user state, rewrites env-scoped paths for the new managed root, and clears only live runtime residue like locks, pid files, and sockets. If `openclaw` is already available on `PATH`, it also binds the imported env to an env-local migrated launcher so you can keep using it through OCM immediately.

When a configured agent workspace is a repository checkout outside the plain
OpenClaw home, including through a symlink, migration copies that workspace into
the new environment and rewrites the imported config to use the copy. The source
home and repository remain unchanged. Config `$include` files still must remain
inside the plain home because OCM does not take ownership of external config.

Environment clone, export, and import flows preserve managed OpenClaw plugin
payloads under the legacy, extension, npm, and Git install roots. Clone and
import still clear live sessions, logs, backups, and process residue so the new
environment does not share active runtime state with its source.

Clone, import, and migration give the target environment a new local gateway and MCP app sandbox listener. They do not copy a public `mcp.apps.sandboxOrigin` because that URL belongs to the source environment's external routing and may still reach the source sandbox. Direct connections derive the target sandbox port automatically. For a target behind a reverse proxy or tunnel, pass its dedicated public origin explicitly:

```bash
ocm env clone source target --sandbox-origin https://target-apps.example.com
ocm env import ./source.ocm-env.tar --name target --sandbox-origin https://target-apps.example.com
ocm migrate target --sandbox-origin https://target-apps.example.com
```

If the config root, `mcp`, `mcp.apps`, or `mcp.apps.sandboxOrigin` is owned by OpenClaw's `$include`, flatten that section before clone, import, or migration. OCM fails closed instead of flattening include-owned configuration or leaving a copied source origin active.

`adopt inspect` and `adopt plan` are the explicit read-only preview tools. Use them when you want to inspect the plain OpenClaw home OCM would read or preview the target env/root before importing.

### Keep supervised envs visible

```bash
ocm service status
ocm logs mira --tail 50
ocm logs mira -f
```

OCM negotiates fresh-process restart support only when it executes an
`openclaw.mjs` entrypoint directly or through OCM's managed Node.js toolchain,
so the gateway PID is the process OCM owns. `ocm service status <env>` reports
`protocol v1` when OpenClaw can hand restart intent back to OCM atomically.
With that protocol, `ocm service restart <env>` asks OpenClaw to restart
immediately through its recovery handoff. OpenClaw records eligible active
sessions and subagents before exiting, OCM starts the replacement gateway, and
OpenClaw resumes that recoverable work after startup. This does not wait for an
in-flight turn to finish before replacing the gateway process.

Package-manager, shell, host-Node, and other wrapper-backed bindings run in
legacy compatibility mode without OCM's native service identity or detached
respawn. `ocm service restart <env>` preserves their existing direct-supervisor
restart behavior and warns that in-flight work cannot be recovered. Bind a
directly invoked OpenClaw runtime to gain recovery-aware restarts. Use
`ocm service restart <env> --force` only to explicitly bypass a recovery
handoff that is advertised but unhealthy.

## Why not just run OpenClaw directly?

Running OpenClaw directly is fine for the simplest case.

Use `ocm` when you want:

- more than one environment
- clean runtime and launcher separation
- stable and local-dev setups side by side
- inspectable supervised env gateways
- safer upgrades, snapshots, and repair flows

Manual setup works. `ocm` is what makes it feel organized.

On Unix and Windows, updates to an already-private OpenClaw config preserve its
private access. OCM creates the replacement privately and verifies protection
before writing data; files with other authored access rules keep the existing writer.

## Learn more

For the full guide, including scenarios and command details, see [docs/USAGE.md](docs/USAGE.md).

You can also use:

```bash
ocm help
ocm help dev
ocm help start
ocm help setup
ocm help env
ocm help release
ocm help runtime
ocm help launcher
ocm help service
```

## Platform support

Current support:

- macOS
- Linux

Background services:

- macOS uses `launchd`
- Linux uses `systemd --user`

Windows service support is not implemented yet.

## License

See [LICENSE](LICENSE).
