# Release prerequisites

For automatic post-merge version PRs and signed release dispatch, see
[Automatic OCM releases](AUTOMATIC_RELEASES.md). The same signing, CI and complete
asset checks below apply to both automatic and manual releases.

The canonical `.github/workflows/release.yml` workflow publishes OCM releases from signed tags on `main`. Linux packaging needs no repository credentials. Each macOS matrix job imports one Developer ID Application certificate, signs `ocm` with the identifier `com.openclaw.ocm`, submits a temporary ZIP to Apple's notary service, checks the executable's notarization ticket, and verifies the executable again after extracting the final tarball.

Configure these GitHub Actions repository secrets:

- `APPSTORE_CERTIFICATES_FILE_BASE64`: a base64-encoded PKCS#12 file containing the Developer ID Application certificate and private key.
- `APPSTORE_CERTIFICATES_PASSWORD`: the PKCS#12 export password.
- `APPSTORE_API_PRIVATE_KEY`: the App Store Connect team API private key in P8 format.
- `APPSTORE_API_KEY_ID`: the matching App Store Connect API key ID.
- `APPSTORE_ISSUER_ID`: the matching App Store Connect issuer ID.

Configure `MACOS_TEAM_ID` as a GitHub Actions repository variable. It must contain the 10-character Apple Developer Team ID for the certificate. The release fails if the signature has another identifier or team, is ad-hoc, lacks the hardened runtime or secure timestamp, or does not pass Apple's notarization checks.

Use the same Apple Developer team and `com.openclaw.ocm` identifier for every release. Certificate renewal within that team preserves the stable designated requirement macOS uses to identify OCM across updates. Changing the team or identifier causes macOS to treat the executable as different code.

OCM keeps the existing `.tar.gz` install and self-update format. Apple cannot staple tickets to a standalone command-line executable or tar archive. The workflow therefore notarizes a ZIP containing the signed executable and verifies the ticket with `codesign --verify --strict --check-notarization --test-requirement '=notarized'` before packaging. `--check-notarization` forces an online ticket check, and the explicit `notarized` requirement makes a missing ticket fail validation. Unlike `spctl --assess --type execute`, this checks standalone command-line tools without requiring an app bundle. The Mach-O code signature is embedded in the executable, and `package-release.sh` verifies it after a tar create-and-extract round trip. `install.sh` and `ocm self update` continue to verify the published tarball against `SHA256SUMS` before installing it.

References:

- Apple's `man codesign`: `--check-notarization` and `--test-requirement`.
- [Apple: Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
- [Apple: Customizing the notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)
- [Apple: Packaging Mac software for distribution](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution)
- [Apple: Code Signing Tasks](https://developer.apple.com/library/archive/documentation/Security/Conceptual/CodeSigningGuide/Procedures/Procedures.html)
- [GitHub: Installing an Apple certificate on macOS runners](https://docs.github.com/actions/how-tos/deploy/deploy-to-third-party-platforms/sign-xcode-applications)

## Homebrew

The formula in [openclaw/homebrew-tap](https://github.com/openclaw/homebrew-tap)
uses the published, checksummed release archives for macOS ARM64, macOS x86_64,
and Linux x86_64. The tap's existing scheduled reconciliation discovers stable
releases; OCM does not need a cross-repository publishing token or dispatch hook.
Do not add Linux ARM64 until the release workflow publishes and tests that target.

Homebrew owns upgrades to its installed executable. Use
`brew upgrade openclaw/tap/ocm`; `ocm self update --check` can still inspect release
availability. The initial formula installs v0.2.39, which predates the ownership
guard. The guard takes effect only after the next approved OCM release reaches
the tap, not when this source change merges.

## crates.io

The Cargo package is `openclawocm`; the executable and library remain `ocm`.
The registry's existing `ocm` crate is unrelated. Renaming this package does not
publish it or change its version. Historical binary-release verification accepts
both package names, but crate publication requires `openclawocm`.

`.github/workflows/publish-crate.yml` is manual-only. Dispatch it on `main` after
the complete binary release for the **same signed tag** has been published.
It verifies the canonical repository, signed tag, protected-main ancestry,
version PR, package identity, published release, and exact-SHA CI. A job without
OIDC permission packages and builds the crate with `cargo publish --dry-run`;
the publishing job revalidates the release before obtaining its short-lived
registry token. The existing binary-release and automatic-release workflows
do not dispatch crate publication.

### Initial publication and ownership

Trusted publishing cannot create a new crate: crates.io requires an API token
for the first publication. This is a separate, explicitly approved release step.
Do not publish the existing v0.2.39 tag: its package name is still `ocm`.

1. Select the next approved signed release that includes the `openclawocm`
   package, and complete its binary release first.
2. From an up-to-date, trusted `main` checkout, verify that tag and create a
   separate checkout of the returned commit:

   ```bash
   tag=v<approved-version>
   source="$(./scripts/verify-crate-release.sh openclaw/ocm "$tag")"
   git worktree add --detach ../ocm-crate-release "$source"
   cd ../ocm-crate-release
   cargo package --list --locked --package openclawocm
   cargo publish --dry-run --locked --package openclawocm --registry crates-io
   ```

3. A maintainer with a verified crates.io account must authenticate locally with
   a short-lived publish API token, then run
   `cargo publish --locked --package openclawocm --registry crates-io` from that
   verified checkout. Do not add the bootstrap token as a repository secret.
4. Verify the published version and add the intended OpenClaw maintainers or
   GitHub team as crate owners. Confirm the actual team slug and membership
   before using `cargo owner --add github:openclaw:<team-slug> openclawocm`.
   Keep an individual owner who can administer ownership; team owners have
   more limited permissions. Do not remove the bootstrap owner before the new
   owners have accepted and verified access.

### Trusted publisher settings

In the `openclawocm` crate's **Settings > Trusted Publishing**, add GitHub with:

| Field | Value |
| --- | --- |
| Repository owner | `openclaw` |
| Repository name | `ocm` |
| Workflow filename | `publish-crate.yml` |
| Environment | `crates-io` |

The GitHub repository environment `crates-io` must allow deployment from the
`main` **branch only**, not arbitrary branches or tags. Preserve any existing
reviewer requirements. The workflow has no long-lived token fallback.
After the initial publication and ownership/trusted-publisher configuration,
revoke the bootstrap token.

For each subsequent approved release, publish the binary release first, then:

```bash
gh workflow run publish-crate.yml --repo openclaw/ocm --ref main -f tag=v<approved-version>
```

Verify the workflow result and the exact version on crates.io. A failed dispatch
does not authorize changing the tag, disabling verification, or republishing
different source under the same version.

References:

- [crates.io trusted publishing](https://crates.io/docs/trusted-publishing)
- [Cargo publishing and crate ownership](https://doc.rust-lang.org/cargo/reference/publishing.html)
