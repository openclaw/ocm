# Changelog

All notable changes to OCM are documented here.

## Unreleased

## 0.3.8 - 2026-10-29

**Highlights:** Safer environment copies and recovery, stronger runtime ownership, and reliable Windows runtime installation and launch.

### Fixed

- Preserve committed SQLite state when environment roots, internal shared-state directories, or registered agent paths use aliases. Capture closed WAL databases in read-only directories, and preserve user attributes when snapshotting read-only files on Unix. ([#750](https://github.com/openclaw/ocm/pull/750), [#780](https://github.com/openclaw/ocm/pull/780), [#767](https://github.com/openclaw/ocm/pull/767), [#748](https://github.com/openclaw/ocm/pull/748))
- Keep copied Git worktrees, shared checkouts, and bare repositories usable after the source is removed when their required Git data is included in the copy. Relocate copied metadata and object-storage links without pulling external bare-repository donors into an export. ([#749](https://github.com/openclaw/ocm/pull/749), [#773](https://github.com/openclaw/ocm/pull/773), [#793](https://github.com/openclaw/ocm/pull/793))
- Preserve directory routes needed to reach registered development sources' Git configuration and alternate object storage during environment removal. Keep scoped restore compatible with those protected dependencies. ([#766](https://github.com/openclaw/ocm/pull/766), [#799](https://github.com/openclaw/ocm/pull/799))
- Preserve independent directories renamed only by case during scoped restore, and list snapshots for the correct environment when names share a case-insensitive storage directory. ([#802](https://github.com/openclaw/ocm/pull/802), [#778](https://github.com/openclaw/ocm/pull/778))
- Refuse export-output collisions without replacing independently created files, and keep parent-directory replacements from introducing external bytes into exports. ([#752](https://github.com/openclaw/ocm/pull/752), [#769](https://github.com/openclaw/ocm/pull/769))
- Reject environment and snapshot prune ages outside the supported date range before removing data. ([#775](https://github.com/openclaw/ocm/pull/775), [#783](https://github.com/openclaw/ocm/pull/783))
- Build linked runtimes and companion packages through aliased OCM roots, preserving native dependency versions, private bindings, shared package identity, and host dependencies. ([#737](https://github.com/openclaw/ocm/pull/737), [#751](https://github.com/openclaw/ocm/pull/751), [#762](https://github.com/openclaw/ocm/pull/762))
- Reclaim packing scratch after interrupted local builds while preserving files still held by live packing processes. ([#717](https://github.com/openclaw/ocm/pull/717))
- Avoid Windows stack overflow when hashing runtime trees, launch installed package entrypoints through Node.js, and verify native companion entrypoint paths correctly. ([#729](https://github.com/openclaw/ocm/pull/729), [#734](https://github.com/openclaw/ocm/pull/734), [#757](https://github.com/openclaw/ocm/pull/757))
- Preserve Windows package junctions through runtime publication and recovery, and create private directories correctly on long ordinary Windows paths. ([#759](https://github.com/openclaw/ocm/pull/759), [#761](https://github.com/openclaw/ocm/pull/761))
- Reject stale service starts after runtime removal, binding changes, launcher replacement, or environment recreation with a different port. ([#755](https://github.com/openclaw/ocm/pull/755), [#772](https://github.com/openclaw/ocm/pull/772), [#785](https://github.com/openclaw/ocm/pull/785), [#803](https://github.com/openclaw/ocm/pull/803))
- Retain managed runtimes while foreground commands use them. Preserve recreated environments during dev-switch rollback and independently reinstalled runtimes during simulation cleanup. ([#787](https://github.com/openclaw/ocm/pull/787), [#784](https://github.com/openclaw/ocm/pull/784), [#753](https://github.com/openclaw/ocm/pull/753))
- Prevent batch upgrades from blocking on aliased locks, and keep asynchronous jobs and upgrade history associated with their own environments when names share case-insensitive storage. ([#782](https://github.com/openclaw/ocm/pull/782), [#798](https://github.com/openclaw/ocm/pull/798), [#800](https://github.com/openclaw/ocm/pull/800))
- Exit quietly when log-pipeline readers close while still reporting real output-flush failures and unreadable selected logs, including environments named like daemon logs. ([#754](https://github.com/openclaw/ocm/pull/754), [#765](https://github.com/openclaw/ocm/pull/765), [#801](https://github.com/openclaw/ocm/pull/801))
- Handle Linux process observations for exited tasks without treating them as running or authorizing signals against them. ([#704](https://github.com/openclaw/ocm/pull/704))

### Changed

- Refresh Git configuration support to gix-config 0.62.1 while retaining Rust 1.88 support, and update the pinned Rust cache and Python setup actions. Thanks @dependabot (#706, #707).
  ([#738](https://github.com/openclaw/ocm/pull/738))
- Update the embedded certificate roots to webpki-roots 1.0.11, removing emSign C1/C3 authorities from OCM downloads. Connections whose only authority is a retired root now fail verification. Thanks @joshavant. ([#704](https://github.com/openclaw/ocm/pull/704))
- Isolate upgrade test fixtures from host launcher tools and wait for native packing fixture processes to finish before cleanup. ([#788](https://github.com/openclaw/ocm/pull/788), [#789](https://github.com/openclaw/ocm/pull/789))

### Upgrade notes

Install the updated OCM executable to receive these fixes and the revised certificate roots. Existing OpenClaw environments are not automatically upgraded. Supported native downloads remain Linux x64 and macOS ARM64/x64; the Windows fixes apply to Windows source builds. See [usage guidance](docs/USAGE.md) for the preserved scope of copies, exports, snapshots, and cold upgrade finalization.

## 0.3.7 - 2026-10-27

**Highlights:** Reliable migration and scoped recovery, preserved local extension dependencies, and upgrade simulations that verify the selected target.

### Added

- Start a stopped borrowed dev environment from another checkout and switch it to that worktree while preserving its environment data. Active sessions still require matching source and port choices. ([#617](https://github.com/openclaw/ocm/pull/617), [#657](https://github.com/openclaw/ocm/pull/657), [#709](https://github.com/openclaw/ocm/pull/709))

### Fixed

- Preserve implicit external default workspaces through both migration and adoption, with the imported configuration selecting the copied data and the source left unchanged. ([#719](https://github.com/openclaw/ocm/pull/719))
- Restore scoped checkpoints across read-only captured ancestors without changing live ancestor permissions or independent project contents. Preserve full snapshots of symlinked roots, safely clear read-only transient residue, and reject snapshot storage that overlaps a custom source root. ([#721](https://github.com/openclaw/ocm/pull/721), [#607](https://github.com/openclaw/ocm/pull/607), [#695](https://github.com/openclaw/ocm/pull/695), [#701](https://github.com/openclaw/ocm/pull/701))
- Preserve native npm's installed dependency versions and their transitive bindings when materializing selected source extensions, including the directory-copy fallback. ([#722](https://github.com/openclaw/ocm/pull/722))
- Honor environment-backed plugin policies, source manifest aliases, native slot values and authorized configuration include roots in targeted local builds. ([#585](https://github.com/openclaw/ocm/pull/585), [#589](https://github.com/openclaw/ocm/pull/589), [#593](https://github.com/openclaw/ocm/pull/593), [#606](https://github.com/openclaw/ocm/pull/606))
- Reject a simulation target replaced after preparation instead of reporting another release as successfully validated. Keep simulation checks on their original environment, handle channels sharing a release version, and prevent local-repo simulation installs from writing dependencies into the user's checkout. ([#723](https://github.com/openclaw/ocm/pull/723), [#697](https://github.com/openclaw/ocm/pull/697), [#651](https://github.com/openclaw/ocm/pull/651), [#663](https://github.com/openclaw/ocm/pull/663))
- Reject blocking OpenClaw finalization receipts before publishing a replacement runtime, including capability-consent errors returned with a successful exit status, while preserving nonblocking plugin warnings. Thanks @hannesrudolph (#132).
- Preserve environment bindings when runtime or launcher metadata is unreadable, and skip dev environments correctly during bulk upgrades. ([#696](https://github.com/openclaw/ocm/pull/696), [#679](https://github.com/openclaw/ocm/pull/679))
- Migrate admitted home aliases, copied plugins and internal absolute links without writing back into the source. Retain durable plugin dependencies and skill files whose names resemble runtime residue. ([#588](https://github.com/openclaw/ocm/pull/588), [#603](https://github.com/openclaw/ocm/pull/603), [#604](https://github.com/openclaw/ocm/pull/604), [#658](https://github.com/openclaw/ocm/pull/658), [#675](https://github.com/openclaw/ocm/pull/675))
- Retain shared skills, ClawHub tracking and saved skill revisions when copying or cleaning environments. Keep ordinary export archives free of credentials and device identity omitted by ordinary import, exclude archive staging files from their own contents, and stage imports privately. ([#605](https://github.com/openclaw/ocm/pull/605), [#609](https://github.com/openclaw/ocm/pull/609), [#615](https://github.com/openclaw/ocm/pull/615), [#630](https://github.com/openclaw/ocm/pull/630), [#672](https://github.com/openclaw/ocm/pull/672), [#654](https://github.com/openclaw/ocm/pull/654), [#618](https://github.com/openclaw/ocm/pull/618))
- Protect active environments during pruning, registered source Git configuration during cleanup, and live credentials and sessions from deletion based on unrelated path text. ([#660](https://github.com/openclaw/ocm/pull/660), [#614](https://github.com/openclaw/ocm/pull/614), [#665](https://github.com/openclaw/ocm/pull/665))
- Reuse a healthy Gateway on repeated start, avoid recreating an environment removed concurrently, clean up launchers after refused startup, and retain onboarding guards when root options precede the command. ([#649](https://github.com/openclaw/ocm/pull/649), [#656](https://github.com/openclaw/ocm/pull/656), [#669](https://github.com/openclaw/ocm/pull/669), [#646](https://github.com/openclaw/ocm/pull/646))
- Stop admitting new Gateways after daemon shutdown is requested. Preserve foreground command cleanup after background-to-foreground job control and report signal termination consistently. ([#720](https://github.com/openclaw/ocm/pull/720), [#718](https://github.com/openclaw/ocm/pull/718), [#673](https://github.com/openclaw/ocm/pull/673), [#664](https://github.com/openclaw/ocm/pull/664))
- Recognize healthy Gateways on custom local and pinned tailnet addresses, and render TLS-aware URLs and accurate foreground-dev status. ([#592](https://github.com/openclaw/ocm/pull/592), [#608](https://github.com/openclaw/ocm/pull/608), [#610](https://github.com/openclaw/ocm/pull/610), [#670](https://github.com/openclaw/ocm/pull/670), [#634](https://github.com/openclaw/ocm/pull/634))
- Honor Linux's configured XDG service directory, quote service paths containing spaces, and report unavailable user service managers accurately. ([#699](https://github.com/openclaw/ocm/pull/699), [#674](https://github.com/openclaw/ocm/pull/674), [#633](https://github.com/openclaw/ocm/pull/633))
- Keep undated startup errors visible, stream continuous undated output promptly, follow logs created after attachment, and separate recreated environments from old service log history. ([#662](https://github.com/openclaw/ocm/pull/662), [#694](https://github.com/openclaw/ocm/pull/694), [#666](https://github.com/openclaw/ocm/pull/666), [#668](https://github.com/openclaw/ocm/pull/668))
- Exclude current supervisor diagnostic identities from environment logs and refresh those exclusions in already-attached followers. Preserve independently owned history and prevent child-log aliases from recreating diagnostics during rotation; fully unrecorded historical aliases remain outside the attribution guarantee. ([#702](https://github.com/openclaw/ocm/pull/702), [#703](https://github.com/openclaw/ocm/pull/703), [#708](https://github.com/openclaw/ocm/pull/708), [#724](https://github.com/openclaw/ocm/pull/724))
- Preserve shell activation include roots and caller variables, select the evaluating shell correctly, validate explicit shell names, and handle Fish paths containing newlines. Keep launcher execution scoped when login profiles export other OpenClaw paths. ([#587](https://github.com/openclaw/ocm/pull/587), [#626](https://github.com/openclaw/ocm/pull/626), [#642](https://github.com/openclaw/ocm/pull/642), [#681](https://github.com/openclaw/ocm/pull/681), [#698](https://github.com/openclaw/ocm/pull/698), [#676](https://github.com/openclaw/ocm/pull/676))
- Preserve authorized includes in dev startup and resolve foreground-dev relative includes from the source checkout. ([#590](https://github.com/openclaw/ocm/pull/590), [#591](https://github.com/openclaw/ocm/pull/591))
- Reclaim abandoned runtime and private Node preparation stages, refuse automatic installs through linked custom pnpm stores, preserve the installed executable if installer publication fails, and refuse self-update when daemon ownership is uncertain. ([#667](https://github.com/openclaw/ocm/pull/667), [#700](https://github.com/openclaw/ocm/pull/700), [#611](https://github.com/openclaw/ocm/pull/611), [#659](https://github.com/openclaw/ocm/pull/659), [#612](https://github.com/openclaw/ocm/pull/612))

### Changed

- Refresh JSON5 and ZIP handling to json5 1.3.4 and zip 9.1.2, plus compatible Git and build dependency patches, while retaining Rust 1.88 support.
- Refresh 28 compatible Rust dependencies, including ICU 2.4, ureq 3.5, plist 1.10.3, and Git configuration support, while retaining Rust 1.88 support.

### Upgrade notes

See [usage guidance](docs/USAGE.md) for stopped dev checkout switching, independent upgrade paths, migration, and export modes. Use `--include-state` when an export must preserve the environment's complete state; ordinary exports retain their documented selective scope.

## 0.3.6 - 2026-10-22

**Highlights:** Safer recovery and migration, TLS-aware Gateway readiness, reliable targeted builds, and refreshed managed Node.js.

### Added

- Store a separate supervisor bootstrap-log directory through `OCM_DAEMON_LOG_DIR`, preserving it across service refresh and upgrade while keeping Gateway logs in their existing locations. ([#150](https://github.com/openclaw/ocm/pull/150))

### Fixed

- Stop rejected Gateways before snapshot compensation replaces their files, retain recovery data when shutdown cannot be confirmed, and preserve dangling environment-root links during rollback. ([#548](https://github.com/openclaw/ocm/pull/548), [#549](https://github.com/openclaw/ocm/pull/549))
- Preserve replacement environments and runtimes borrowed by other environments when an earlier upgrade simulation cleans up. ([#547](https://github.com/openclaw/ocm/pull/547), [#554](https://github.com/openclaw/ocm/pull/554))
- Keep migration target ownership through import and failure cleanup, relocate admitted nested absolute configuration includes, and preserve included settings during minimum setup. ([#552](https://github.com/openclaw/ocm/pull/552), [#551](https://github.com/openclaw/ocm/pull/551), [#545](https://github.com/openclaw/ocm/pull/545))
- Exclude an export archive from its own contents, including when its destination is reached through a directory alias. ([#546](https://github.com/openclaw/ocm/pull/546))
- Preserve linked pack and loose Git object storage required by registered development sources during admission and cleanup. ([#553](https://github.com/openclaw/ocm/pull/553), [#558](https://github.com/openclaw/ocm/pull/558))
- Honor disabled plugins, accept native aliases and case variants, and resolve selected extension workspace dependencies in targeted local builds without rewriting source manifests. ([#544](https://github.com/openclaw/ocm/pull/544), [#550](https://github.com/openclaw/ocm/pull/550), [#555](https://github.com/openclaw/ocm/pull/555))
- Recognize healthy TLS Gateways using verified local certificates and the supervised configuration, with bounded configuration lookup and helper cleanup. ([#556](https://github.com/openclaw/ocm/pull/556))
- Keep Windows development sessions alive while status readers retain session snapshots, and use the selected source working directory for entry paths rejected by hardened Windows Node.js. ([#569](https://github.com/openclaw/ocm/pull/569), [#568](https://github.com/openclaw/ocm/pull/568))

### Changed

- Refresh the private Node.js toolchain to 24.22.1, reject stale cached patches, and prepare the selected private runtime even when reusing an already-installed OpenClaw package. Compatible host Node.js selection remains unchanged. ([#568](https://github.com/openclaw/ocm/pull/568))

### Upgrade notes

Refresh an older private Node.js cache while online before relying on offline launches; see [package runtime Node.js selection](docs/USAGE.md#package-runtime-nodejs-selection). Refreshing a bound runtime can restart its Gateway asynchronously. Use a maintenance window and wait for normal service readiness before checking health.

## 0.3.5 - 2026-10-20

**Highlights:** Prevent cross-environment data loss during restore and cleanup, preserve imported state, and refresh compatible dependencies.

### Fixed

- Refuse snapshot restores and upgrade rollbacks that would replace another registered environment's complete root or access links, including legacy nested and temporarily missing roots. Thanks @shakkernerd (#526, #527).

- Preserve registered child environments and borrowed or temporary dev sources during removal and health cleanup, including shared-clone Git object storage. Thanks @shakkernerd.

- Preserve configured agent data, workspace folders, session references, and Git ownership when importing, cloning, or migrating environments. Thanks @shakkernerd.

- Preserve nested and absolute configuration includes and SQLite symlinks in state imports and ordinary exports. Thanks @shakkernerd.

- Retain uncertain service ownership after daemon replacement or failed cleanup, preserve newer restart requests, and keep sibling gateways unchanged during targeted operations. Thanks @shakkernerd.

- Restore ordinary restart for settled inactive gateways. Thanks @shakkernerd.

- Preserve shell builtin semantics for source, dot, eval, and command launchers, keep activated shells alive, and reuse launchers with equivalent relative working directories. Thanks @shakkernerd.

- Reach onboarding before background service startup for fresh environments, honor JSON5 and included Gateway ports, reject invalid ports, and prevent upgrade hangs for environments named `batch`. Thanks @shakkernerd.

- Select a compatible Node.js version for each OpenClaw package, respect configured dev UI dependency directories, and retain captured checkout identity when source aliases change. Thanks @shakkernerd.

### Changed

- Move macOS CI and release builds to Xcode 27, preserve Intel validation with Rosetta and existing deployment targets, and add a nonpublishing release dry run.

- Refresh twelve compatible Rust dependencies, including zip 9.0.5, json5 1.3.3, and libc 0.2.191, while retaining Rust 1.88 support.

- Update zip to 9.0.4 and refresh the pinned Rust toolchain installer action while retaining Rust 1.88 support.

- Refresh eleven compatible Rust dependencies, including rustls 0.23.46, serde_json 1.0.154, and zip 9.0.3, while retaining Rust 1.88 support.

- Keep strict native notarization verification on disposable macOS runners while using the provider-recommended TCP fallback for ticket lookups. Thanks @shakkernerd.

- Isolate the supervisor publication-retry regression from listening sockets retained by parallel pre-exec fixtures.

## 0.3.4 - 2026-10-15

These cumulative notes shipped by v0.3.4 and were previously left under Unreleased; they are preserved here as a historical rollup.

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

### Added

- Add opt-in `env export --include-state` to retain the environment installation with coherent SQLite snapshots, restore plugin receipts and legacy data, and preserve configuration files verbatim.

- Add Unix `env artifact export` for bounded, raw file reads beneath an environment
  home, rejecting unsafe paths and observed file changes without choosing the
  caller's destination.
- Make `ocm self update` a recoverable local transaction that retains the previous executable, survives caller disconnection, refreshes a running daemon, checks gateway recovery, and records a durable result. Add `--status` and `--recover` for interrupted updates.
- Prepare `@openclaw/ocm` npm distribution of signed Rust binaries, with a
  no-download launcher, exact platform aliases, and manual OIDC publishing.
  Include every npm CI lane in automatic-release verification.
- Let `runtime build-local` assemble repeatable, commit-matched official companion plugins through OpenClaw's release packaging contract, stage them as trusted runtime-owned bundled plugins with isolated dependencies, and record artifact and entrypoint hashes for verification.

### Changed

- Refresh seven compatible Rust dependencies, including cc 1.7.0 and smallvec 1.16.3, while retaining Rust 1.88 support.
- Update `libc` to 0.2.190 and the pinned Rust installer action to retry transient official-server checksum failures, retaining Rust 1.88 support.
- Update `cc` to 1.5.1, `find-msvc-tools` to 0.1.14, and `smallvec` to 1.16.2 while retaining Rust 1.88 support.
- Define ordered remote development checks, isolated checkout transfer, and proof reuse in the contributor guide. Thanks @shakkernerd (#290).
- Update fresh managed Node.js installs and CI to 24.21.0 while preserving cached 24.15.0 toolchains across OCM upgrades.
- Document live npm and Homebrew availability in v0.2.40, observed one-time
  bootstrap recovery, and the manual ARM payload's provenance exception.
- Keep package-manager distribution on npm and Homebrew only; disable Cargo
  publication while retaining source installation and the internal `ocm` name.
- Refresh Rust dependencies and pinned CI actions while preserving Rust 1.88 support.
- Make `ocm service restart <env>` restart immediately through OpenClaw's protocol-v1 recovery handoff so eligible interrupted sessions and subagents resume after startup, preserve the legacy direct-restart behavior with a warning when recovery is unavailable, keep `--force` as an explicit bypass for an unhealthy handoff, and avoid self-restart deadlocks.
- Resolve and package the complete transitive closure of private OpenClaw `workspace:*` dependencies during `runtime build-local`, rewriting nested workspace specs only inside scratch archives so current source graphs install without mutating the checkout.

### Fixed

- Relocate session, recorded file, worktree, and other text references across registered OpenClaw databases during state-inclusive import, with per-database counts and unchanged binary payloads.

- Relocate installed plugin receipts and runtime state references when importing a state-inclusive archive, preserving other state and references outside the source environment. Share path-component and file-URL matching with plain-home migration so sibling path prefixes are preserved.

- Rewrite env-scoped configuration paths when importing a state-inclusive archive so agent workspaces and directories point into the imported environment, while preserving state contents.

- Disclose skipped candidate preparation and validation in upgrade dry-run text and JSON, including batch previews, and point operators to isolated simulation. Thanks @shakkernerd (#124).
- Limit runtime metadata to 64 MiB of decoded JSON, including gzip expansion, and preserve installed runtime files and metadata when an oversized update source is rejected. Thanks @SebTardif (#117).
- Preserve newer Stop and Uninstall requests after foreground development sessions, and refuse foreground startup while a superseding request leaves the background Gateway running or requested. Thanks @shakkernerd (#299, #310).
- Keep runtime binding admission isolated through upgrade finalization and rollback so a newly attached sibling cannot receive another environment's uncheckpointed downgrade. Thanks @shakkernerd (#296, #309).
- Preserve Unix shell `exec` semantics for unquoted managed-service launcher recipes. Thanks @shakkernerd (#300, #308).
- Report delayed native restart rejection as a failed service action even while the original Gateway remains healthy. Thanks @shakkernerd (#298, #307).
- Preserve Skill Workshop proposal hashes, support files, and rollback evidence during home import and runtime cleanup. Thanks @Patrick-Erichsen (#245).
- Reject mixed or malformed candidate preflight failures instead of treating an unsupported-check marker as success. Thanks @shakkernerd for the report and @goutamadwant for the fix (#128, #165).
- Accept reformatted and binary macOS LaunchAgent plists for the same OCM store while still rejecting foreign or invalid owners. Thanks @TheAngryPit (#147, #149).
- Reject linked environment archive roots and metadata before import or legacy snapshot restore can read files outside the extracted archive.
- Keep npm-owned executable updates with npm, prevent temporary npx caches from
  owning background services, and preserve process identity during npm-launched
  gateway refreshes. Protect managed and symlinked installer destinations even
  with `--force`.
- Refuse mutating `ocm self update` for Homebrew-owned executables and direct
  users to `brew upgrade openclaw/tap/ocm`; keep release checks available.
- Keep unrelated supervised gateways on their persisted child specifications when an environment is destroyed, removed, or pruned, so latent metadata or process-environment drift cannot restart sibling gateways during cleanup.
- Resolve `TMPDIR` from the live OCM daemon whenever a gateway is spawned, create a daemon-scoped private temporary directory, and fall back to a private directory under `/tmp` instead of persisting a per-login path across reboots.

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
