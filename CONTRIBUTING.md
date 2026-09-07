# Contributing to OCM

Start with [README.md](README.md) for OCM's workflow and
[docs/USAGE.md](docs/USAGE.md) for command behavior. Keep changes focused on one
problem and include enough context to reproduce it.

## Development and checks

Install Rust 1.88 or newer, Cargo, and rustfmt. The minimum supported version is
declared in [Cargo.toml](Cargo.toml); [CI](.github/workflows/ci.yml) checks that
version and Windows compilation, and runs tests on stable Rust on Linux and
macOS.

Run these checks from the repository root:

```sh
cargo fmt --check
cargo check --workspace --all-targets --locked
cargo test --locked
```

For a focused integration test, use its filename without `.rs`:

```sh
cargo test --locked --test cli_invocation_tests
```

CI also checks installation into a temporary prefix. To reproduce that smoke
check without replacing an installed `ocm`:

```sh
install_root="$(mktemp -d)"
cargo install --locked --path . --root "$install_root"
"$install_root/bin/ocm" --version
```

The npm launcher integration tests need Node.js `^22.15.0 || >=24.0.0`.
Packaging tests use Python 3.13 and npm 11.19.0, with a private local fixture
registry, cache, prefix, and home:

```sh
python3 -B -m unittest discover -s scripts/tests -p 'test_npm_*.py'
```

These tests do not publish packages or change installed services. CI also
checks npm installation of the existing signed v0.2.39 binaries on all three
release platforms; that fixture proves byte preservation, not support for
publishing v0.2.39 through the new workflow.

Tests use [tests/support/mod.rs](tests/support/mod.rs) for temporary directories,
isolated `HOME` and `OCM_HOME`, and service fixtures. Reuse those helpers; do not
point tests at a real environment or service. Manual state-changing experiments
also need isolated state and must not disturb an operator's running services.
Keep credentials, private configuration, and checkpoint data out of fixtures
and public logs.

For automatic-release script changes, the additional Python test command and
its prerequisites are in [docs/AUTOMATIC_RELEASES.md](docs/AUTOMATIC_RELEASES.md).

## Issues and pull requests

Check existing issues and pull requests before starting overlapping work. Bug
reports should include the OCM version, OS, reproducing steps, expected behavior,
and actual behavior, with secrets and personal paths removed.

Use the [PR template](.github/pull_request_template.md) and keep **Allow edits
from maintainers** enabled. Titles use `type: description`, with an optional
scope when useful, such as `fix(auth): ...`. Accepted types are `feat`, `fix`,
`improve`, `refactor`, `docs`, and `chore`. Link a fixed issue with `Closes #123`.
Explain user impact and provide relevant tests or other evidence, including
limitations. Required CI must pass before merging.

## Release work

Release preparation and publication are separate from ordinary contributions.
Do not bump versions, publish releases, or change signing credentials or
automation without explicit maintainer authorization. Follow
[docs/RELEASING.md](docs/RELEASING.md) and
[docs/AUTOMATIC_RELEASES.md](docs/AUTOMATIC_RELEASES.md); preserve the existing
signed-release and macOS signing/notarization safeguards.

Coding agents should also read [AGENTS.md](AGENTS.md).
