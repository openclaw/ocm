# Automatic OCM releases

`auto-release.yml` connects merged changes to the existing release pipeline.
It does not update a running OCM daemon or OpenClaw environment. Consumers such
as Odin's scheduled updater still install OCM first, then update OpenClaw.

## Sequence

1. CI completion triggers reconciliation. A scheduled hourly check recovers
   missed events and interrupted runs. Only trusted `main` workflow code runs
   with release credentials; triggering PR code and artifacts are never executed.
2. Reconciliation checks current `main` CI, then examines all changes since the
   current published version. Docs-only changes produce no release. Scripts,
   skills, CI, installers, dependencies and other product changes do.
3. The automation creates one `release/vX.Y.Z` PR changing only the OCM version
   in `Cargo.toml` and `Cargo.lock`. The default increment is a patch. It refreshes
   a stale automation-owned branch with an exact-head lease, then waits for CI.
4. Once the release PR is current, all CI jobs pass and GitHub reports normal
   mergeability, it squash-merges that exact head without bypassing protection.
   No tag is created until the resulting exact `main` commit passes CI too.
5. The configured signer creates a signed annotated tag for that release commit.
   The unchanged `verify-release-tag.sh` and `verify-release-ci.sh` must accept it.
   The existing `release.yml` then builds all platforms, signs/notarizes macOS
   binaries, verifies the complete asset set and publishes the draft release.

Concurrent merges may share one release. Global reconciliation serialization
prevents competing version allocation. Each run re-reads authoritative GitHub
state instead of relying on the triggering event's possibly stale SHA.
Version PR merges do not recursively create new versions. Reconciliation finishes
the pending release before allocating another version.

## One-time configuration

Automatic releases are disabled until `OCM_AUTO_RELEASE_ENABLED` is `true` in
repository Actions variables. Configure the release identity first through
GitHub's protected repository settings, never through chat or logged commands:

- `OCM_RELEASE_TOKEN`: a dedicated release account's fine-grained token scoped
  only to `openclaw/ocm`, with Contents, Pull requests and Actions read/write.
  The account must be allowed to push release branches, merge normally, and
  dispatch workflows, without bypass privileges. Its GitHub-verified email must
  match the signing identity. Repository rules must allow this account's
  version-only PR to merge after the required checks and reviews.
- `OCM_RELEASE_GPG_PRIVATE_KEY`: the release identity's signing-only GPG key,
  whose public key is registered with that GitHub account. GitHub must mark
  annotated tag signatures verified. Keep the key in Actions secrets.
- `OCM_RELEASE_GPG_PASSPHRASE`: its passphrase if applicable.

The workflow uses a SHA-pinned GPG import action that removes imported key
material on exit. The ordinary `GITHUB_TOKEN` remains read-only. A dedicated
account token is necessary because branch/PR/merge events created with
`GITHUB_TOKEN` do not start the CI runs required by the release protocol.
Do not replace the signer with unsigned tags or reuse Apple's code-signing key.
Existing Apple signing secrets and `MACOS_TEAM_ID` remain unchanged.

After configuration, enable the variable and dispatch **Automatic release**
from `main` with `bump=auto`. It will include already-merged unreleased fixes.
Do not mark setup complete until the release PR, signed tag, successful release
workflow and complete public assets have all been observed.

## Version choices and pauses

- Default `auto` and explicit `patch` stop if any unreleased commit declares
  `BREAKING CHANGE:`, `BREAKING-CHANGE:`, or uses a Conventional Commit `!` header.
  Use a manual dispatch with `bump=minor` or `bump=major` after deciding the proper
  compatibility boundary. Undeclared breaking changes cannot be detected; code
  review must enforce release declarations.
- Set a pending release PR to draft or add `release:hold` to pause its merge.
  Set `OCM_AUTO_RELEASE_ENABLED=false` to stop all automatic release operations.
- A manually owned release PR is never adopted, modified or merged. Finish it
  using `scripts/release.sh`, or close it before resuming automation.
- To change a pending release's version increment, close its PR and dispatch
  with the desired different bump. Old branches are not deleted automatically.
  A previously closed proposal is never recreated, even if its branch was
  deleted. Reopen that proposal explicitly to resume the same version.
- Merging a manually prepared version PR does not authorize automatic signing
  or publication. Its owner must complete the existing manual release flow.
- Existing signed tags are never moved. Existing public releases and their
  assets are never rewritten. An active build is not dispatched again.
- A failed or cancelled release build stops reconciliation visibly. Inspect and
  rerun that existing workflow run after correcting the cause. The reconciler
  does not create endless retries or mistake a draft/partial release for success.
- A pushed automation branch whose PR creation was interrupted is recovered only
  when its commit is signed by the release account and contains the exact
  version-only change and automation marker.

## Verification

Run the policy, lifecycle and subprocess boundary tests without public network
access or publishing:

```sh
python3 -B -m unittest discover -s scripts/tests -p 'test_auto_release*.py'
```

These tests run in the existing Linux CI Format job. Cargo, Perl, Git, Python 3,
`gh` and `ssh-keygen` are required. The subprocess proof runs the production CLI
and unchanged release verifiers, pushes real signed tags to an isolated bare
Git repository, and uses real `gh` HTTP requests through its documented Unix
socket transport. The local service verifies the real tag signature before
reporting it verified. It checks successful dispatch and retry deduplication,
plus zero writes for a merged manual release and a closed proposal (with or
without its branch). Temporary keys and state are removed after each test.

GitHub responses and build completion are still service fixtures, not a live
GitHub release or a proof that the production signing account is configured.
Activation therefore still requires the observed live sequence described above.
The existing Rust release-script and release-asset suites remain authoritative
for signed-tag verification and complete-asset publication.
