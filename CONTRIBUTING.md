# Contributing to OCM

Start with [README.md](README.md) for OCM's workflow and
[docs/USAGE.md](docs/USAGE.md) for command behavior. Keep changes focused on one
problem and include enough context to reproduce it.

## Development and checks

Use a task-owned checkout on an approved remote worker from the first check,
including formatting and other short checks. Local work is source inspection,
editing, Git, and transport. Run the commands below in order from the remote
repository root; a passing focused test does not replace a preceding check.
[CI](.github/workflows/ci.yml) remains the source of truth for the supported
toolchain and platform matrix.

### Prepare once

Prepare stable Rust, Cargo, rustfmt, Python 3, Git, Perl, `gh`, `ssh-keygen`, and
Node.js with npm on the remote worker. Use the project's approved tools and
locked dependencies. The Rust minimum is declared in [Cargo.toml](Cargo.toml)
(currently 1.88); CI checks it separately from stable Rust.

For the complete Rust test suite, use CI's Node.js `24.21.0` when available. The
npm launcher accepts `^22.15.0 || >=24.0.0`, while runtime-install tests require
`22.22.3+`, `24.15.0+`, or `25.9.0+`. Their packages are local fixtures, so these
tests do not require registry dependency downloads. A different supported
version is useful development proof, but does not replace the CI version matrix.

Synchronize the intended source, including new files, into the owned remote
checkout before running checks. Record the base, candidate commit (or exact
uncommitted diff), `Cargo.lock`, remote OS, and tool versions with the command
results. Do not synchronize over a running check or copy local build outputs.
Reuse this checkout and its Cargo cache through corrections; one owner controls
its source and `target/` directory. Install missing approved prerequisites on the
worker before starting the sequence, without changing another task's tools or
an installed OCM environment.

For an approved SSH worker with those prerequisites, use Git's source transport
from the local task checkout. Set `OCM_CHECK_HOST` to its existing SSH alias.
Stage intended new files first so they are included in `git diff HEAD`; inspect
the diff for secrets and unrelated changes. The bundle carries committed source
and the patch carries both staged and unstaged edits to tracked files, including
staged additions and deletions. Neither carries ignored Cargo build output.

```sh
set -eu
: "${OCM_CHECK_HOST:?Set an approved SSH worker alias}"
check_stage="$(mktemp -d)"
git bundle create "$check_stage/source.bundle" HEAD
git diff --no-ext-diff --no-textconv --binary HEAD > "$check_stage/source.patch"
check_root="$(ssh "$OCM_CHECK_HOST" 'mktemp -d /tmp/ocm-check.XXXXXX')"
scp "$check_stage/source.bundle" "$check_stage/source.patch" "$OCM_CHECK_HOST:$check_root/"
ssh "$OCM_CHECK_HOST" "git clone '$check_root/source.bundle' '$check_root/repo' &&
  cd '$check_root/repo' && git apply --index --allow-empty ../source.patch &&
  git rev-parse HEAD && git hash-object ../source.patch Cargo.lock"
ssh -t "$OCM_CHECK_HOST" "cd '$check_root/repo' && exec bash"
```

Run the baseline below inside that remote shell. Keep its output outside
`repo/`, under the task's remote directory, together with the printed source
identity. The remote index represents the complete candidate contents; it does
not change the local staging choices. Close the shell after the checks finish.

For a correction, first reconcile any interrupted command and wait until no
check is reading the checkout. Recreate the same local bundle and patch, copy
them to the same remote directory, then refresh only this task-owned checkout:

```sh
set -eu
git bundle create "$check_stage/source.bundle" HEAD
git diff --no-ext-diff --no-textconv --binary HEAD > "$check_stage/source.patch"
scp "$check_stage/source.bundle" "$check_stage/source.patch" "$OCM_CHECK_HOST:$check_root/"
ssh "$OCM_CHECK_HOST" "cd '$check_root/repo' && git fetch ../source.bundle HEAD &&
  git reset --hard FETCH_HEAD && git clean -fd &&
  git apply --index --allow-empty ../source.patch &&
  git rev-parse HEAD && git hash-object ../source.patch Cargo.lock"
```

This discards the previous synchronized source edits and untracked files in
the owned remote checkout. Keep logs outside it; ignored `target/` caches remain
available. Do not use these reset/clean commands in a shared or live checkout.
At completion, copy the needed logs back, close task-owned shells/processes,
then remove only the directories returned by `mktemp` above:
`ssh "$OCM_CHECK_HOST" "rm -rf -- '$check_root'"` and
`rm -rf -- "$check_stage"`. A managed worker or lease also needs its provider's
supported stop operation; deleting the checkout does not release a lease.

### Run the baseline in order

Run this sequence on the remote worker before publishing a coherent change.
`set -eu` stops on the first failed command, leaving later checks unrun.

```sh
set -eu

# 1. Formatting and release-policy/structural checks.
git diff --check HEAD
cargo fmt --check
python3 -B -m unittest discover -s scripts/tests -p 'test_auto_release*.py'

# 2. Compile every workspace target, including test targets.
cargo check --workspace --all-targets --locked

# 3. Run the prescribed Rust suite and isolated install smoke test.
cargo test --locked
install_root="$(mktemp -d)"
trap 'rm -rf -- "$install_root"' EXIT
cargo install --locked --path . --root "$install_root"
"$install_root/bin/ocm" --version
```

There is no separate Clippy gate in the current CI contract. Do not invent one
or equate formatting with compilation. The automatic-release Python suite is
part of every CI Format job, even when the patch does not edit release scripts;
its subprocess prerequisites and isolated fixtures are described in
[automatic releases](docs/AUTOMATIC_RELEASES.md#verification).

During iteration, reproduce a failure with the smallest relevant command, such
as `cargo test --locked --test cli_invocation_tests`. Retain preceding results
only while their source, dependencies, toolchain, and conditions remain valid.
After a correction, rerun the affected checks and finish the unrun remainder in
the same order. A retry of one test does not complete the baseline. Before a
push, inspect the actual candidate and account for every applicable check;
reuse unchanged proof instead of restarting it for a workflow phase change.

### Complete the affected platform and packaging checks

The baseline above proves one worker's OS and toolchain. CI runs the following
additional coverage on every PR. For changes to an affected surface, use the
matching prepared remote environment for early proof; retain other platform
obligations for their actual CI results rather than claiming cross-platform
coverage from one machine.

| Surface | Additional proof and execution conditions |
| --- | --- |
| Rust compatibility | `cargo +1.88.0 check --workspace --all-targets --locked` on Linux with the declared minimum toolchain installed; stable Rust checks and the full test/install sequence on Linux and macOS. |
| Windows paths, privacy, services, or dev lifecycle | Native Windows `cargo check --workspace --all-targets --locked`, then the exact focused Windows commands in the CI `windows` job. Cross-compilation does not execute these cases. |
| macOS private configuration | The ignored cross-user test described below, using its CI invocation on an authorized disposable runner. Ordinary `cargo test` does not execute it. |
| npm packaging, launcher, installation, or recovery | Python 3.13 and npm 11.19.1: `python3 -B -m unittest discover -s scripts/tests -p 'test_npm_*.py'`. CI covers its declared Node.js and Linux/macOS architecture matrix. |
| Signed npm binary installation | CI's `python3 -B scripts/tests/test_npm_install.py --published-binary-fixture` on its Node.js 24 jobs, with the required GitHub access and macOS signing identity. Do not supply signing credentials to an ordinary development checkout. |

The npm packaging suite uses a private local fixture registry, cache, prefix,
and home. It does not publish packages or change installed services. The signed
fixture installs the existing signed v0.2.39 binaries on the three release
platforms; it proves byte preservation, not permission or support for publishing
v0.2.39 through the new workflow.

### Prove the changed behavior and finish

Add the focused reproduction and actual CLI or lifecycle interaction required
by the change. Use the owner-level integration suite and ordinary success/error
controls; do not substitute a test count or successful compilation for the
user-visible result. This additional proof is separate from the baseline and
platform checks. Documentation-only changes need their command, link, and CI
parity checked, without manufacturing a new runtime scenario.

Keep command results bound to the exact candidate. A skipped command remains
unrun. After transport interruption, reconcile the native remote result before
retrying; retain completed checks and partial results while an unknown outcome
remains unresolved. Preparation and transport failures are not product test
failures. Correct the failed layer on the retained worker, and do not broaden a
retry or weaken an assertion to obtain green. Check the final PR head and CI's
tested merge revision before reusing branch proof after target movement.
At the agreed delivery boundary, stop task-owned processes and release owned
remote resources, preserving evidence and any checkout still needed for review.

Tests use [tests/support/mod.rs](tests/support/mod.rs) for temporary directories,
isolated `HOME` and `OCM_HOME`, and service fixtures. Reuse those helpers; do not
point tests at a real environment or service. Manual state-changing experiments
also need isolated state and must not disturb an operator's running services.
Keep credentials, private configuration, and checkpoint data out of fixtures
and public logs.

macOS CI also runs the ignored `private_config_creation_excludes_other_users`
case through passwordless `sudo` after compiling as the normal runner. Only that
case uses privilege, to launch a reader as the existing `nobody` user against a
temporary fixture. The CI command in `.github/workflows/ci.yml` selects the exact
test; it does not run the rest of the suite or any environment service as root.

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
