#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF' >&2
Verify the Developer ID signature on a shipped macOS ocm executable.

Usage:
  scripts/verify-macos-release.sh --binary <path> --team-id <id> [--require-notarization]
EOF
}

binary=""
team_id=""
require_notarization=0
identifier="com.openclaw.ocm"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      shift
      [[ $# -gt 0 ]] || { echo "error: --binary requires a value" >&2; exit 1; }
      binary="$1"
      ;;
    --team-id)
      shift
      [[ $# -gt 0 ]] || { echo "error: --team-id requires a value" >&2; exit 1; }
      team_id="$1"
      ;;
    --require-notarization)
      require_notarization=1
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
[[ "$team_id" =~ ^[A-Z0-9]{10}$ ]] || {
  echo "error: --team-id must be a 10-character Apple Developer Team ID" >&2
  exit 1
}

for tool in codesign grep; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "error: required command not found: $tool" >&2
    exit 1
  }
done

if ! verification="$(codesign --verify --strict --verbose=2 "$binary" 2>&1)"; then
  printf '%s\n' "$verification" >&2
  echo "error: macOS release binary has an invalid code signature" >&2
  exit 1
fi

metadata="$(codesign -d --verbose=4 "$binary" 2>&1)"
if grep -Fxq "Signature=adhoc" <<<"$metadata"; then
  echo "error: macOS release binary has an ad-hoc signature" >&2
  exit 1
fi
grep -Fxq "Identifier=${identifier}" <<<"$metadata" || {
  echo "error: macOS release binary does not use identifier ${identifier}" >&2
  exit 1
}
grep -Fxq "TeamIdentifier=${team_id}" <<<"$metadata" || {
  echo "error: macOS release binary is not signed by Apple Developer Team ${team_id}" >&2
  exit 1
}
grep -Eq '^Authority=Developer ID Application:' <<<"$metadata" || {
  echo "error: macOS release binary is not signed with a Developer ID Application certificate" >&2
  exit 1
}
grep -Eq 'flags=.*\(runtime\)' <<<"$metadata" || {
  echo "error: macOS release binary does not enable the hardened runtime" >&2
  exit 1
}
grep -Eq '^Timestamp=' <<<"$metadata" || {
  echo "error: macOS release binary does not have a secure signing timestamp" >&2
  exit 1
}

if [[ "$require_notarization" == "1" ]]; then
  command -v spctl >/dev/null 2>&1 || {
    echo "error: required command not found: spctl" >&2
    exit 1
  }
  if ! assessment="$(spctl --assess --type execute --verbose=4 "$binary" 2>&1)"; then
    printf '%s\n' "$assessment" >&2
    echo "error: Gatekeeper rejected the macOS release binary" >&2
    exit 1
  fi
  grep -Fq 'source=Notarized Developer ID' <<<"$assessment" || {
    printf '%s\n' "$assessment" >&2
    echo "error: Gatekeeper did not report a notarized Developer ID signature" >&2
    exit 1
  }
fi

echo "Verified macOS release signature: ${identifier} (${team_id})"
if [[ "$require_notarization" == "1" ]]; then
  echo "Verified macOS notarization with Gatekeeper"
fi
