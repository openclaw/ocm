#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "Usage: scripts/verify-published-release.sh <owner/repo> <tag> [expected-commit]" >&2
  exit 1
fi

github() {
  if [[ -n "${OCM_GH_BIN:-}" ]]; then
    "$OCM_GH_BIN" "$@"
  elif command -v ghx >/dev/null 2>&1; then
    ghx --no-cache "$@"
  else
    gh "$@"
  fi
}

repo="$1"
tag="$2"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
verify_args=(--repo "$repo" --tag "$tag")
if [[ -n "${3:-}" ]]; then
  verify_args+=(--commit "$3")
fi
commit="$("${script_dir}/verify-release-tag.sh" "${verify_args[@]}")"

release_data="$(
  github api "repos/${repo}/releases/tags/${tag}" \
    --jq '[.tag_name, (.draft | tostring), (.published_at // "")] | @tsv'
)"
IFS=$'\t' read -r published_tag draft published_at extra <<<"$release_data"
if [[ "$published_tag" != "$tag" || "$draft" != "false" ||
  ! "$published_at" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ||
  -n "${extra:-}" ]]; then
  echo "error: publish the complete binary release for ${tag} before repackaging" >&2
  exit 1
fi
"${script_dir}/verify-release-ci.sh" --repo "$repo" --commit "$commit" >&2

# Recheck the immutable source after the intervening GitHub requests.
"${script_dir}/verify-release-tag.sh" --repo "$repo" --tag "$tag" --commit "$commit"
