# Changelog

All notable changes to OCM are documented here.

## Unreleased

- Use the canonical `openclaw/ocm` repository for installation and self-update
  requests. Thanks @shakkernerd.
- Require exact complete release archives and keep failed release settlement
  unpublished or draft. Thanks @shakkernerd.
- Bind release identity to canonical producer, tag, pull request, CI, and
  protected-main workflow inputs. Thanks @shakkernerd.
- Recheck tag identity and exact-SHA CI inside the publisher before any release
  API mutation. Thanks @shakkernerd.
- Require successful exact-SHA `main` CI before signed release tags can be created or packaged.
- Copy repo-backed and symlinked plain-home agent workspaces into an adopted
  environment and rewrite the imported config to keep the fixture isolated.

### Changed

- Resolve and package the complete transitive closure of private OpenClaw `workspace:*` dependencies during `runtime build-local`, rewriting nested workspace specs only inside scratch archives so current source graphs install without mutating the checkout.
- Materialize environment-targeted local plugins with a valid `node_modules/openclaw` host peer before recording the runtime's full-tree digest.
- Make `ocm service restart <env>` restart immediately through OpenClaw's protocol-v1 recovery handoff so eligible interrupted sessions and subagents resume after startup, preserve the legacy direct-restart behavior with a warning when recovery is unavailable, keep `--force` as an explicit bypass for an unhealthy handoff, and avoid self-restart deadlocks.
- Clean surviving process-group descendants after supervised gateway exit and always reap the process-group leader.
- Migrate direct named Tailscale Service routes during upgrade to an OCM-owned identity proxy and OpenClaw trusted-proxy auth, preserving direct-local password fallback and restoring proxy, route, config, runtime, and service state on rollback.

## 0.2.30 - 2026-07-23

### Added

- Add durable, credential-free upgrade history with observed versions, snapshots, service state, completed stages, and rollback outcomes in pretty, raw, and JSON output.
- Add transactional `ocm upgrade rollback`, including pre-rollback safety snapshots, linked history, strict stale-state and shared-runtime refusal checks, and reversible rollback transactions.
- Retain previous installer-managed runtime bytes after successful in-place upgrades until the matching recovery snapshot is removed or pruned.

### Fixed

- Keep fresh and existing environments compatible across OpenClaw 2026.6.x and 2026.7.x agent configuration shapes, including list and keyed agents, JavaScript key ordering, includes, and identity-bound workspace rewrites.
- Finalize OpenClaw runtime migrations before publishing replacement bindings, repair compatible legacy configuration with the target runtime, verify the reported version and gateway reachability, and restore the previous environment when post-upgrade checks fail.
- Preserve managed plugin payloads and live SQLite state through snapshots, clones, imports, exports, upgrades, and rollback while excluding generated dependencies, runtime residue, SQLite sidecars, and other nonportable state.
- Update the managed Node.js fallback to 24.15.0, enforce OpenClaw's supported host Node.js ranges, and preserve the managed toolchain on execution, npm lifecycle, service, restart-handoff, simulation, and Windows `Path` flows.
- Reject unsafe OpenClaw downgrades before mutation, prevent direct changes to bound runtimes, isolate shared-runtime upgrades, and serialize runtime binding and upgrade preflight decisions.
- Serialize each complete environment upgrade operation so concurrent mutations cannot publish stale bindings, snapshots, service policy, or history.
- Make failed rollback and snapshot removal cleanup consistent by validating snapshot identity, tombstoning metadata before archive staging, removing unrecorded safety snapshots, and preserving post-removal warnings.
- Retry transient executable-busy failures when invoking launchd or systemd service managers without masking other service-manager errors.

## 0.2.29 - 2026-07-21

### Fixed

- Preserve every workspace selected by effective OpenClaw agent configuration across snapshots, exports, clones, imports, migrations, cleanup, and upgrade rollback while rejecting unsafe or externally owned paths.
- Retry an environment archive transaction once when a child entry disappears during traversal, while keeping repeated disappearance and every other filesystem error fatal.
- Keep MCP App sandbox listener ports aligned when gateway ports are reassigned, rewrite coupled loopback origins, and preserve public origins during in-place repair.
- Prevent cloned, imported, and migrated environments from inheriting a source public MCP App sandbox origin, accept an explicit target origin for proxy-backed environments, and fail closed when the setting is owned by `$include`.

## 0.2.28 - 2026-07-16

### Fixed

- Run directly supervised gateways through OpenClaw's external restart-handoff contract, keep wrapper-backed and legacy runtimes in no-respawn compatibility mode, and prevent child commands from inheriting the shared OCM service identity.
- Prepare release versions through squash pull requests before tagging protected `main`, and start publication automatically from the signed tag push.
- Run release validation once and move pinned CI actions to their current Node 24 runtimes.

## 0.2.27 - 2026-07-12

### Fixed

- Let `runtime build-local` package unreleased OpenClaw `main` checkouts with local workspace dependencies through the repo-provided npm adapter, and report the failing command tail instead of early build chatter.
- Prevent `runtime build-local` from reconciling or replacing an OpenClaw checkout's dependencies while running its package build.
- Reject an already-running shared OCM daemon when its service definition belongs to another `OCM_HOME`, and remove the shared service binding after the last running environment stops or is uninstalled.
- Reuse an identical healthy official runtime when concurrent installers publish it first instead of failing the waiting command.

## 0.2.26 - 2026-07-12

### Fixed

- Match environment-owned processes by path components so destroy never targets sibling-prefix directories.
- Replace copied OpenClaw config symlinks with environment-owned files without rewriting external targets.
- Serialize environment cleanup with other mutations so repairs cannot save stale bindings or race destroy.
- Add atomic state-token guards for automation that previews and destroys environments.
- Keep automatically assigned gateway ports stable across create, show, list, and status output.
- Give cloned and imported environments independent service state and collision-free gateway ports.
- Serialize environment registry transactions so concurrent lifecycle commands preserve every entry and allocate distinct ports.
- Preserve environment roots and recovery snapshots when dev worktree cleanup fails during destroy.
- Keep auto-assigned ports stable without overriding a later gateway port written by OpenClaw configuration.
- Reject non-UTF-8 process input cleanly, require version flags to stand alone, and terminate quietly when a pipeline closes its output.
- Prevent Fish activation injection through crafted paths and clear stale OpenClaw controls when switching environments.
- Keep tables within narrow terminal widths by truncating headers or falling back to compact rows.
- Verify self-update release digests and staged binary versions before replacement, compare releases with SemVer precedence, clean temporary artifacts on failure, and reject Linux ARM64 until release assets are published.
- Reject launcher deletion while environments still depend on it, and route shell-expanding launcher recipes through a shell instead of lossy direct execution.
- Accept the standard human-output flags for `runtime releases` and document `runtime build-local` in the command map.
- Keep plain-home migration path rewrites scoped to the imported OpenClaw state and reject overlapping source and target roots before mutation.
- Fail migration planning on corrupt environment registries and only auto-bind executable OpenClaw commands from `PATH`.
- Report launcher and environment rollback failures alongside the original migration error instead of hiding partial cleanup.
- Make release preparation rollback-safe, verify signed annotated tags on every resume path, and publish branch and tag refs atomically.
- Build locked release artifacts only from a GitHub-verified tag matching the package version, require the complete supported target matrix before publication, publish a versioned checksum-verifying installer, pin CI actions, and test the declared Rust 1.88 minimum.
- Preserve uncommitted dev worktrees and reject unrelated checkout collisions by requiring exact Git worktree registration before reuse or removal.
- Make single and merged log following rotation-safe and byte-safe, preserving split or invalid UTF-8, multiline record ordering, and boundaries around unterminated records.
- Prevent overlapping source watches, remove the unleased override mutation path, reclaim stale watch locks, preserve terminal job control, handle termination signals, and restore background-service policy when takeover cannot stop or exits with an error.
- Verify managed Node.js archives against pinned release checksums and serialize bootstrap so concurrent processes cannot delete a valid toolchain.
- Preserve working runtimes until replacement artifacts, dependencies, and metadata are ready; require release integrity and reject unsafe artifact names.
- Write private service definitions, preserve existing service-directory modes, escape systemd specifiers, reject directive injection, and fail closed on unknown launchd users or cross-store service ownership.
- Serialize supervisor restart requests, restore service policy after failed reconciliation, preserve raw warnings, and fail upgrades when a gateway restart cannot be observed.
- Preserve every recognized npm release channel when multiple dist-tags point to the same OpenClaw version.
- Keep exact-version release guidance pinned to the selected version instead of a mutable channel.
- Make release-validation runs package-shaped, commit-pinned, concurrency-safe, credential-aware, and explicit about session-preserving fixtures and owned cleanup.
