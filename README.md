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

Install the latest release:

```bash
curl -fsSL https://github.com/openclaw/ocm/releases/latest/download/install.sh | bash
```

Install a specific release:

```bash
curl -fsSL https://github.com/openclaw/ocm/releases/download/v<ocm-version>/install.sh | bash -s -- --version v<ocm-version>
```

Update an existing install:

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
ocm dev shaks --watch
ocm dev shaks --watch --force
ocm dev shaks --repo /path/to/openclaw --watch --force
ocm dev shaks --service
ocm dev shaks --onboard
```

`dev` creates or reuses an isolated env, provisions an OpenClaw worktree under the repo's own `.worktrees/`, bootstraps the minimum local config so the gateway can run immediately, and then starts the gateway in the foreground. `--root` lets you place that env anywhere. `--watch` keeps a source-run gateway rebuilding in place. While watch is active, `ocm @<env> -- ...`, `ocm env run <env> -- ...`, `ocm env resolve <env> -- ...`, service resolution, and `ocm env exec <env> -- openclaw ...` use the same watched checkout and run `node <checkout>/openclaw.mjs` directly instead of rebuilding through the package script. `--service` installs and starts the dev env in the OCM background service instead of keeping the process in the current terminal. If a dev env is already running in the background, `--watch --force` temporarily takes it over for the watch session and restores the background service when watch exits. For an existing runtime or launcher env, `--repo <path> --watch --force` temporarily runs that source checkout against the env's real root, config, state, and port without changing its binding, tees foreground output to the env gateway logs, then restores a running background service when watch exits. `--onboard` runs local onboarding first and then starts the dev gateway. If OCM cannot infer the repo on the first run, it asks once for the OpenClaw repo path and then reuses that repo for later dev envs.

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
New snapshots preserve the complete environment root, including credentials,
browser profiles, plugin payloads, unknown future directories, modes, symlinks,
and SQLite sidecars. OCM verifies tree contents and SQLite integrity before
publishing the checkpoint. APFS uses copy-on-write clones when available;
other filesystems use a metadata-preserving full copy. Restore discards only
explicit process residue such as locks, sockets, PIDs, and temporary runtime
directories. Legacy tar snapshots remain readable.

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
migrations; returning to an older release requires a complete snapshot captured
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
Once an environment is bound to a runtime, direct `runtime update`,
`runtime install --force`, `runtime build-local --force`, and `runtime remove`
operations reject that runtime. Use `ocm upgrade <env>` so the environment gets
the snapshot, OpenClaw migration, rollback, and verification path, or clear the
binding first when intentionally managing an unused runtime.

For unreleased OpenClaw workspaces, `runtime build-local` follows the complete
transitive closure of private `workspace:*` packages. It rewrites nested
workspace specs only inside scratch archives before installation; the source
checkout remains unchanged. Environment-targeted local plugins that peer-depend
on `openclaw` receive a runtime-local host link before full-tree hashing.

When a named Tailscale Service routes directly to an environment gateway,
`upgrade` can migrate it after the verified checkpoint to an OCM-owned identity
proxy and OpenClaw trusted-proxy auth. The stable Service hostname is retained,
direct-local password fallback is preserved, and same-host Service hairpins are
attributed to the verified tailnet owner so the gateway host can use the stable
URL. Rollback restores the prior config, route, proxy service, runtime, and
service policy.

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
ocm dev luna --watch
ocm dev existing-env --repo ~/src/openclaw --watch --force
```

Use `ocm dev` when you want an isolated source-run checkout with its own env root and gateway port, or when you want to temporarily run source against an existing env in watch mode without rebinding it. While watch mode is running, OCM's OpenClaw-running commands for that env resolve to the watched checkout, so one-shot checks use the same built `openclaw.mjs` that the watcher is maintaining. Existing-env watch output is saved under that env's `.openclaw/logs/` directory, so `ocm logs <env>` remains useful while the foreground watcher is running. If you are already inside an OpenClaw checkout, `ocm setup` can detect that and suggest a local path automatically.

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
