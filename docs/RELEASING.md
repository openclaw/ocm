# Release prerequisites

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
