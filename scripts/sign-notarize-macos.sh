#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF' >&2
Sign and notarize a macOS ocm executable in the release workflow.

Usage:
  scripts/sign-notarize-macos.sh --binary <path>

Required environment:
  OCM_MACOS_TEAM_ID
  OCM_NOTARY_API_PRIVATE_KEY
  OCM_NOTARY_API_KEY_ID
  OCM_NOTARY_ISSUER_ID
EOF
}

binary=""
identifier="com.openclaw.ocm"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      shift
      [[ $# -gt 0 ]] || { echo "error: --binary requires a value" >&2; exit 1; }
      binary="$1"
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
  shift
done

[[ -n "$binary" ]] || { echo "error: --binary is required" >&2; exit 1; }
[[ -f "$binary" && ! -L "$binary" ]] || {
  echo "error: macOS release binary is missing or invalid: $binary" >&2
  exit 1
}

required_environment=(
  OCM_MACOS_TEAM_ID
  OCM_NOTARY_API_PRIVATE_KEY
  OCM_NOTARY_API_KEY_ID
  OCM_NOTARY_ISSUER_ID
)
for name in "${required_environment[@]}"; do
  [[ -n "${!name:-}" ]] || {
    echo "error: required environment variable is empty: $name" >&2
    exit 1
  }
done
[[ "$OCM_MACOS_TEAM_ID" =~ ^[A-Z0-9]{10}$ ]] || {
  echo "error: OCM_MACOS_TEAM_ID must be a 10-character Apple Developer Team ID" >&2
  exit 1
}

for tool in codesign ditto plutil security xcrun; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "error: required command not found: $tool" >&2
    exit 1
  }
done

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
identities="$(
  security find-identity -v -p codesigning |
    awk -v team="(${OCM_MACOS_TEAM_ID})" '
      /Developer ID Application:/ && index($0, team) { print $2 }
    '
)"
identity_count="$(grep -c . <<<"$identities" || true)"
if [[ "$identity_count" != "1" ]]; then
  echo "error: expected exactly one Developer ID Application identity for team ${OCM_MACOS_TEAM_ID}, found ${identity_count}" >&2
  exit 1
fi
identity="$identities"

codesign \
  --force \
  --identifier "$identifier" \
  --options runtime \
  --sign "$identity" \
  --timestamp \
  "$binary"

"${script_dir}/verify-macos-release.sh" \
  --binary "$binary" \
  --team-id "$OCM_MACOS_TEAM_ID"

tmp_dir="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/ocm-notarize.XXXXXX")"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT
chmod 700 "$tmp_dir"

notary_key="${tmp_dir}/AuthKey.p8"
submission_archive="${tmp_dir}/ocm.zip"
submission_result="${tmp_dir}/submission.json"
umask 077
printf '%s\n' "$OCM_NOTARY_API_PRIVATE_KEY" >"$notary_key"
ditto -c -k --keepParent "$binary" "$submission_archive"

if ! xcrun notarytool submit "$submission_archive" \
  --key "$notary_key" \
  --key-id "$OCM_NOTARY_API_KEY_ID" \
  --issuer "$OCM_NOTARY_ISSUER_ID" \
  --wait \
  --output-format json >"$submission_result"; then
  echo "error: Apple notarization submission failed" >&2
  exit 1
fi

submission_status="$(plutil -extract status raw -o - "$submission_result")"
submission_id="$(plutil -extract id raw -o - "$submission_result")"
if [[ "$submission_status" != "Accepted" ]]; then
  echo "error: Apple notarization ${submission_id:-unknown} finished with status ${submission_status:-unknown}" >&2
  exit 1
fi
echo "Apple notarization accepted submission ${submission_id}"

"${script_dir}/verify-macos-release.sh" \
  --binary "$binary" \
  --team-id "$OCM_MACOS_TEAM_ID" \
  --require-notarization
