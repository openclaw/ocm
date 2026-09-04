# Recoverable local self-update

`ocm self update` checks the release archive digest and executable version before
changing the install. It retains the previous executable beside the installed
binary, then starts that retained OCM as a local helper. The helper atomically
replaces the CLI, refreshes a running daemon, and checks the daemon version and
HTTP readiness of previously running managed gateways. It leaves the persisted
desired service state intact. An absent or stopped daemon stays stopped.

The refresh briefly interrupts gateways, including a gateway hosting the caller.
The helper runs in a separate session with detached standard streams, so it does
not need the caller to remain connected. On Linux, a helper inside the OCM
systemd service first moves into a transient `systemd-run --user --scope` scope.
If that handoff fails, OCM does not replace the installed executable.

## Result and recovery

After reconnecting, use the same `HOME` and `OCM_HOME`:

```sh
ocm self update --status
```

The JSON receipt reports the transaction ID, phase, versions, affected gateways,
original error, and any rollback error. It contains no saved process environment.
The receipt and retained executable live in `.<binary-name>.self-update` beside
the executable. For an executable named `ocm`, that directory is
`.ocm.self-update`. The command prints its recovery executable path before waiting.

If activation or health checks fail, the helper restores the previous CLI and
refreshes the daemon back to that version. `rolledBack` means recovery succeeded,
not that the update succeeded. `rollbackFailed` retains the original error and
the recovery error. Both produce a nonzero update exit status.

If the helper crashes or the host reboots during the update:

```sh
ocm self update --recover
```

Recovery rolls back, rather than guessing whether to finish an interrupted
publication. A repeated recovery of a terminal receipt makes no service changes.
If the candidate CLI cannot execute, invoke the printed retained executable:

```sh
/path/to/bin/.ocm.self-update/previous self update --recover
```

Only one update or recovery can own an executable at a time, even across OCM
homes. An unfinished receipt blocks a fresh update. A surviving service-manager
subcommand retains the update lock after a helper crash; recovery is refused
until that command exits. OCM never kills an unknown PID to reclaim a lock.

## Boundaries

- `--check` and unchanged releases do not restart services.
- Recovery is local and explicit after a helper crash or reboot. This is not a
  boot-time recovery service or a zero-downtime update.
- The previous executable and latest receipt remain until the next admitted
  update. This is not a release history or a manual downgrade facility.
- OCM refuses unknown or already-skewed running daemon versions, another store's
  daemon, and service definitions bound to another executable. Resolve that
  existing state before updating. Customized service definitions can require a
  separate service refresh before admission.
- The helper preserves current gateway desired state. It does not change
  runtime bindings, gateway configuration, snapshots, or OpenClaw versions.
- Supported release targets remain macOS x86_64/arm64 and Linux x86_64.
  Windows and Linux arm64 self-update are explicitly unsupported; ordinary OCM
  behavior on those platforms is unchanged.
- Linux gateway-hosted updates require a working user systemd manager and
  `systemd-run`. Logout, machine failure, storage corruption, and third-party
  process containment are not transparent-survival guarantees. Recovery files
  remain available after interruption.
- Service-manager calls use the existing lifecycle implementation. If the
  service manager hangs, the receipt remains nonterminal and the lock remains
  held; OCM does not report success or start a competing update.

Embedded macOS signatures are copied unchanged. Release digests, archive entry
checks, and exact candidate version checks remain required. No new dependency,
remote host, credential flow, or generic lifecycle transaction framework is used.
