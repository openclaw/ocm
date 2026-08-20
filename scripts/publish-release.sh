#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: scripts/publish-release.sh --repo <owner/repo> --tag <tag> --asset-dir <dir>" >&2
}

repo=""
tag=""
asset_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      shift
      [[ $# -gt 0 ]] || { echo "error: --repo requires a value" >&2; exit 1; }
      repo="$1"
      ;;
    --tag)
      shift
      [[ $# -gt 0 ]] || { echo "error: --tag requires a value" >&2; exit 1; }
      tag="$1"
      ;;
    --asset-dir)
      shift
      [[ $# -gt 0 ]] || { echo "error: --asset-dir requires a value" >&2; exit 1; }
      asset_dir="$1"
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
  shift
done

[[ -n "$repo" ]] || { usage; exit 1; }
[[ -n "$tag" ]] || { usage; exit 1; }
[[ -n "$asset_dir" ]] || { usage; exit 1; }

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
"${script_dir}/prepare-release-assets.sh" "$asset_dir" >/dev/null

lookup_error="$(mktemp "${TMPDIR:-/tmp}/ocm-release-lookup.XXXXXX")"
cleanup() {
  rm -f "$lookup_error"
}
trap cleanup EXIT

if release_state="$(gh release view "$tag" --repo "$repo" --json isDraft --jq '.isDraft' 2>"$lookup_error")"; then
  if [[ "$release_state" != "true" ]]; then
    echo "error: release ${tag} is already public; refusing to replace published assets" >&2
    exit 1
  fi
elif grep -Fxq 'release not found' "$lookup_error"; then
  gh release create "$tag" \
    --repo "$repo" \
    --draft \
    --verify-tag \
    --title "$tag" \
    --generate-notes
else
  cat "$lookup_error" >&2
  echo "error: failed to inspect existing release ${tag}" >&2
  exit 1
fi

asset_names=(
  "ocm-aarch64-apple-darwin.tar.gz"
  "ocm-x86_64-apple-darwin.tar.gz"
  "ocm-x86_64-unknown-linux-gnu.tar.gz"
  "install.sh"
  "SHA256SUMS"
)
assets=()
for name in "${asset_names[@]}"; do
  assets+=("${asset_dir}/${name}")
done
gh release upload "$tag" --repo "$repo" --clobber "${assets[@]}"

expected_assets="$(printf '%s\n' "${asset_names[@]}" | sort)"
actual_assets="$(gh release view "$tag" --repo "$repo" --json assets --jq '.assets[].name' | sort)"
if [[ "$actual_assets" != "$expected_assets" ]]; then
  echo "error: draft release assets are incomplete; leaving ${tag} as a draft" >&2
  printf 'expected:\n%s\nactual:\n%s\n' "$expected_assets" "$actual_assets" >&2
  exit 1
fi

release_flags=(--draft=false)
version_without_build="${tag#v}"
version_without_build="${version_without_build%%+*}"
if [[ "$version_without_build" == *-* ]]; then
  release_flags+=(--prerelease --latest=false)
else
  release_flags+=(--latest)
fi
gh release edit "$tag" --repo "$repo" "${release_flags[@]}"
