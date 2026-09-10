# OCM Command Cookbook

Use these commands as patterns. Run `ocm help <surface>` when flags may have
changed.

## Inspect State

```sh
ocm env list
ocm env show <env>
ocm env status <env>
ocm service status
ocm service status <env>
ocm runtime list
ocm runtime show <runtime>
ocm runtime verify <runtime>
```

## Run OpenClaw Inside An Env

```sh
ocm @<env> -- --version
ocm @<env> -- status
ocm @<env> -- doctor --fix
ocm @<env> -- plugins list
ocm @<env> -- models list
ocm @<env> -- dashboard
ocm @<env> -- tui
```

For non-OpenClaw commands inside the env:

```sh
ocm env exec <env> -- env
ocm env exec <env> -- node --version
ocm env exec <env> -- sh -lc 'pwd && env | sort'
```

## Create Or Start Envs

Runtime-backed env from an installed runtime:

```sh
ocm start <env> --runtime <runtime>
```

Runtime-backed env from a published version:

```sh
ocm start <env> --version 2026.4.24
ocm start <env> --channel stable
ocm start <env> --channel beta
```

Foreground/no-service env:

```sh
ocm start <env> --runtime <runtime> --no-service
```

Interactive onboarding path:

```sh
ocm start <env> --runtime <runtime> --onboard
```

Local command launcher env:

```sh
ocm start <env> --command 'pnpm openclaw' --cwd /path/to/openclaw
ocm start <env> --command 'pnpm openclaw' --cwd /path/to/openclaw --no-service
```

## Dev Checkout Envs

Use for source-backed development and reproductions. Do not use this as proof
of release packaging.

```sh
ocm dev <env> --repo /path/to/openclaw
ocm dev <env> --repo /path/to/openclaw --service
ocm dev <env> --watch
ocm dev <env> --watch --force
ocm dev <env> --onboard
ocm dev status
ocm dev status <env>
```

New dev envs use the exact explicit or enclosing checkout with separate state;
they create no Git worktree. Existing envs keep their recorded source, including
legacy OCM-owned worktrees. Rerun `ocm dev <env>` after editing or pulling that
source. A resumed borrower reports missing tooling for explicit preparation.
Removing it preserves the checkout and dependencies.

## Runtime Management

Install a published runtime:

```sh
ocm runtime install --channel stable
ocm runtime install --channel beta
ocm runtime install --version 2026.4.24
```

Build a release-shaped runtime from local source:

```sh
ocm runtime build-local <runtime> --repo /path/to/openclaw --force
ocm runtime verify <runtime>
ocm runtime show <runtime>
node "$HOME/.ocm/runtimes/<runtime>/files/node_modules/openclaw/openclaw.mjs" --version
```

Update or remove runtimes:

```sh
ocm runtime update <runtime>
ocm runtime update --all
ocm runtime remove <runtime>
```

## Upgrade Flows

Dry-run:

```sh
ocm upgrade <env> --dry-run
ocm upgrade <env> --version 2026.4.24 --dry-run
ocm upgrade <env> --runtime <runtime> --dry-run
```

Real upgrade:

```sh
ocm upgrade <env>
ocm upgrade <env> --channel beta
ocm upgrade <env> --version 2026.4.24
ocm upgrade <env> --runtime <runtime>
```

Simulation matrix:

```sh
ocm upgrade simulate <env> --to 2026.4.24 --scenario all
ocm upgrade simulate <env> --to beta --scenario all
ocm upgrade simulate <env> --to /path/to/openclaw --scenario all
ocm upgrade simulate <env> --to /path/to/openclaw --scenario all --keep-simulations
```

## Existing-User Test Flow

Clone first when the test does not need sessions, logs, or backups. A clone
retains durable auth/settings and is therefore secret-bearing. Keep its service
stopped until credentials are replaced with mocks or dedicated test accounts,
or the user explicitly authorizes real external access.

Clone does not copy dev source bindings.

```sh
ocm env clone <existing-env> <test-env>
ocm start <test-env> --no-service
ocm service status <test-env>
ocm logs <test-env> --tail 100
```

For session/history validation, copy the source env's `.openclaw` directory to
a task-owned temporary path and use `ocm adopt import --name <test-env>
<copied-openclaw-home>`. Assert the expected session fixture exists before the
upgrade. Do not use `ocm env clone` for that case because clone intentionally
clears sessions, logs, and backups.

Exercise:

```sh
ocm @<test-env> -- status
ocm @<test-env> -- doctor --fix
ocm @<test-env> -- plugins list
ocm @<test-env> -- plugins update --all --dry-run
ocm @<test-env> -- tui
```

Upgrade the clone:

```sh
ocm upgrade <test-env> --version 2026.4.24
ocm upgrade <test-env> --runtime <local-runtime>
```

## Fresh-User Test Flow

```sh
ocm start <fresh-env> --runtime <runtime>
ocm service status <fresh-env>
ocm @<fresh-env> -- --version
ocm @<fresh-env> -- status
ocm @<fresh-env> -- doctor --fix
ocm @<fresh-env> -- plugins list
ocm @<fresh-env> -- plugins update --all --dry-run
ocm @<fresh-env> -- models list
ocm @<fresh-env> -- tui
```

## Plugins

```sh
ocm @<env> -- plugins --help
ocm @<env> -- plugins list
ocm @<env> -- plugins list --json
ocm @<env> -- plugins inspect <plugin-id> --json
ocm @<env> -- plugins install /path/to/plugin-dir
ocm @<env> -- plugins install /path/to/plugin.tgz
ocm @<env> -- plugins install <plugin-spec>
ocm @<env> -- plugins update <plugin-id> --dry-run
ocm @<env> -- plugins update <plugin-id>
ocm @<env> -- plugins update --all --dry-run
```

Check both manifest data and `package.json`-derived metadata when testing
plugins that declare information in both places.

## Services And Logs

Use OCM service commands for supervised gateway lifecycle:

```sh
ocm service status
ocm service status <env>
ocm service install <env>
ocm service start <env>
ocm service stop <env>
ocm service restart <env>
ocm service uninstall <env>
```

Logs are env-first:

```sh
ocm logs <env>
ocm logs <env> --tail 100
ocm logs <env> --stream error --tail 100
ocm logs <env> --follow
```

## Migration And Portability

Plain `~/.openclaw` into OCM:

```sh
ocm adopt inspect ~/.openclaw
ocm adopt plan --name <env> ~/.openclaw
ocm adopt import --name <env> ~/.openclaw
```

One-step migration:

```sh
ocm migrate <env> ~/.openclaw
```

Export/import OCM envs:

```sh
ocm env export <env> --output ./<env>.ocm-env.tar
ocm env import ./<env>.ocm-env.tar --name <new-env>
```

## Cleanup

Preview before destroying:

```sh
ocm env destroy <env>
```

Destroy:

```sh
ocm env destroy <env> --yes
```

Protected env:

```sh
ocm env destroy <env> --yes --force
```

Check for leftovers:

```sh
ocm env list
ocm service status
git -C /path/to/openclaw worktree list
```
