# AGENTS.md

OCM is the Rust CLI for managing OpenClaw environments, runtimes, launchers, and
services. Read [README.md](README.md) for the product workflow and
[CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks, and pull requests.
`AGENTS.md` is canonical; `CLAUDE.md` is a relative symlink to this file.

## Working in this repository

- Inspect `git status -sb` before edits. Preserve unrelated changes and use a
  task-owned branch or worktree.
- Read the affected source and tests before changing behavior. CLI handlers and
  rendering live in `src/cli/`; environment, runtime, service, and store behavior
  live in their corresponding `src/` modules.
- Use Cargo and the Rust version declared in `Cargo.toml`. Keep `Cargo.lock`
  changes scoped to intentional dependency or version changes.
- Use `cargo fmt --check`, `cargo check --workspace --all-targets --locked`, and
  focused tests such as `cargo test --locked --test cli_invocation_tests`.
  [CI](.github/workflows/ci.yml) defines the full platform checks.
- Reuse `TestDir`, `ocm_env`, and the service fixtures in
  [tests/support/mod.rs](tests/support/mod.rs). Keep test homes, OCM state, and
  subprocesses isolated from installed environments and services.
- Follow the [PR template](.github/pull_request_template.md). Describe the
  problem, solution, impact, and actual proof; identify checks not run.

## Safety and releases

- Do not commit credentials, private configuration, checkpoint contents, or
  personal machine details. Use synthetic fixtures and redact public evidence.
- Do not stop, restart, migrate, or clean up a real OCM/OpenClaw environment
  without explicit operator approval.
- Code-change or merge authority does not authorize a version bump, signed tag,
  release publication, or changes to release credentials and automation.
- Follow [release prerequisites](docs/RELEASING.md) and
  [automatic releases](docs/AUTOMATIC_RELEASES.md) for authorized release work.
  Preserve the signed-tag, CI, and macOS signing/notarization checks.
