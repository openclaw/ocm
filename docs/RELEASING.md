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
availability. Since v0.2.40, OCM refuses mutating self-updates of
Homebrew-owned executables.

## npm

`@openclaw/ocm` is one public npm package. Its normal version exposes the `ocm`
launcher; exact optional dependency aliases select platform versions of the
same package, such as `V-darwin-arm64`. Only macOS ARM64/x64 and Linux x64 glibc
are supported. There are no install hooks, binary downloads during installation,
or Rust compiler requirements. See [npm usage](../npm/README.md).

The first npm release, v0.2.40, is published with the npm launcher and native
ownership guard. The workflow rejects v0.2.39 and other older sources. Future
version bumps, signed releases, and publications still require explicit
maintainer authorization.

### Account setup

An npm maintainer with write access to the `openclaw` scope must separately
approve and perform the first legitimate package publication. npm requires an
existing package before a trusted publisher can be configured.

The one-time bootstrap is complete for `@openclaw/ocm`; do not repeat it.
For a new package, use the exact tarballs from successful workflow preparation
and three-platform validation. The workflow has no prepare-only switch: wait
for its publication attempt to finish and inspect the failed step and registry
state before any manual publication. Authentication errors are not absence.
Bootstrap only one verified platform payload under its
`platform-latest-<platform>` tag (or `platform-next-<platform>` for a prerelease)
using an authenticated maintainer account with 2FA, `--ignore-scripts`,
`--provenance=false`, and `--access public`. Do not publish a placeholder,
manually move `latest`, or put a bootstrap token in GitHub.

Configure a GitHub trusted publisher in the package's npm settings:

- Organization: `openclaw`
- Repository: `ocm`
- Workflow filename: `publish-npm.yml`
- Environment: `npm`
- Permission: allow direct `npm publish`, not only staged publishing

Publisher configurations created since September 3, 2026 default to staging
permission in npm's new settings UI. Explicitly allow direct publication.
Alternatively, with npm >=11.15.0, an authenticated account with 2FA and package
write access can run:

```sh
npm trust github @openclaw/ocm --repo openclaw/ocm --file publish-npm.yml --env npm --allow-publish
```

This changes account-side configuration; it is not a dry run. Configure the
publisher only after the package exists. One configuration covers the root
and all platform versions because they share the same registry package name.

On GitHub, the `npm` environment must allow only the `main` branch, not tags
or arbitrary branches. Keep any existing required reviewers and protections.
The workflow uses GitHub-hosted runners, Node 24, npm 11.19.0, read-only GitHub
permissions, and `id-token: write` only in the publication job. There is no
`NPM_TOKEN` or `NODE_AUTH_TOKEN` fallback.

After a real OIDC publication and provenance verification, set npm publishing
access to **Require two-factor authentication and disallow tokens**:

```sh
npm access set mfa=publish @openclaw/ocm --registry=https://registry.npmjs.org
```

Complete the account's 2FA approval. Do not use `mfa=automation`, which permits
token overrides. Verify the setting in npm's package settings; `npm access get
status` reports visibility, not MFA enforcement. Revoke only a dedicated
temporary bootstrap token, if one was created. Preparation and local registry
tests do not prove that account-side trust or hardening is configured.

### First publication: v0.2.40

On September 7, 2026, the ARM bootstrap created both its explicit
`platform-latest-darwin-arm64` tag and an implicit `latest` pointing to
`0.2.40-darwin-arm64`. Removing `latest` returned HTTP 400. These are observed
bootstrap results, not a guarantee for other packages. Do not repeat the
deletion or republish an accepted version.

The full package metadata briefly returned 404 after the successful publish,
while the exact-version endpoint and tarball were available. Reconcile an
accepted publication with bounded exact-version, tarball-integrity, ownership,
and dist-tag checks; a transient metadata 404 alone is not permission to retry.

After trust configuration and integrity verification, only the failed job in
the [original workflow run](https://github.com/openclaw/ocm/actions/runs/34113713509/attempts/2)
was rerun, reusing its tested artifacts. The unchanged publisher accepted the
matching ARM payload, published the remaining platforms, then published the
stable root last. Its existing version-order and channel guards allowed the
platform prerelease to advance to `latest=0.2.40`; no tag workaround was needed.

The manually bootstrapped `0.2.40-darwin-arm64` payload has a registry signature
and contains the original signed, notarized binary, but has **no OIDC
provenance**. The root, Intel macOS, and Linux versions were published through
OIDC and have verified provenance. Binary signing and registry signatures do
not replace an npm provenance statement.

### Publishing and recovery

After the binary release is complete, dispatch `publish-npm.yml` on `main`
with its signed `v` tag. Preparation verifies the canonical signed source,
exact-main CI, complete release inventory, original `SHA256SUMS`, and GitHub
asset digests. It packages the tag's launcher and already signed native bytes
without rebuilding or modifying the executable. Every native platform installs
the exact prepared tarballs; macOS also verifies signature and notarization.

All versions come from the verified Cargo version, preserving the existing
two-file version-bump contract. npm versions with build metadata or a reserved
`-darwin-arm64`, `-darwin-x64`, or `-linux-x64` suffix are rejected.

The protected publication job revalidates source and assets, publishes the
platform versions serially under `platform-latest-<platform>` or
`platform-next-<platform>`, then publishes the root under `latest` for stable
versions or `next` for prereleases. Every channel is preflighted before any write
and rechecked immediately before publication. Package-wide concurrency prevents competing
workflow runs. Do not run concurrent manual publishing against this package.

A retry accepts an existing version only when its SHA-512 integrity matches
the exact prepared tarball. HTTP errors other than 404 are not absence.
Matching completed older releases are no-ops and never roll a channel back.
If the root version exists but its channel needs repair, the workflow stops:
inspect the published bytes and current channel, then perform separately
approved maintainer tag recovery. OIDC is not a token fallback for `npm dist-tag`
or account-management commands.

npm's automatic provenance identifies the protected packaging workflow's
`GITHUB_SHA`. It does not necessarily identify the native compilation commit.
Each tarball includes `release.json` with the signed tag, native source commit,
and release asset digests; payloads also include the executable SHA-256. These
are package contents covered by its digest, not extra automatically generated
SLSA materials. Do not override provenance variables or dispatch from an
unprotected tag to manufacture a matching source identity.

References: [npm trusted publishing](https://docs.npmjs.com/trusted-publishers/),
[npm trust](https://docs.npmjs.com/cli/v12/commands/npm-trust/), and
[npm provenance](https://docs.npmjs.com/generating-provenance-statements/).
