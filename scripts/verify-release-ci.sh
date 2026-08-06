#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: scripts/verify-release-ci.sh --repo <owner/repo> --commit <sha>" >&2
}

github() {
  if [[ -n "${OCM_GH_BIN:-}" ]]; then
    "$OCM_GH_BIN" "$@"
  elif command -v ghx >/dev/null 2>&1; then
    ghx --no-cache "$@"
  elif command -v gh >/dev/null 2>&1; then
    gh "$@"
  else
    echo "error: ghx or gh is required to verify release CI" >&2
    return 1
  fi
}

repo=""
commit=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      shift
      [[ $# -gt 0 ]] || {
        echo "error: --repo requires a value" >&2
        exit 1
      }
      repo="$1"
      ;;
    --commit)
      shift
      [[ $# -gt 0 ]] || {
        echo "error: --commit requires a value" >&2
        exit 1
      }
      commit="$1"
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
  shift
done

if [[ ! "$repo" =~ ^[^/[:space:]]+/[^/[:space:]]+$ ]]; then
  echo "error: invalid GitHub repository: ${repo:-empty}" >&2
  exit 1
fi
if [[ ! "$commit" =~ ^[0-9a-fA-F]{40}$ ]]; then
  echo "error: invalid release commit SHA: ${commit:-empty}" >&2
  exit 1
fi

endpoint="repos/${repo}/actions/workflows/ci.yml/runs?branch=main&event=push&head_sha=${commit}&per_page=20"
run_data="$(
  github api "$endpoint" \
    --jq "(.workflow_runs | map(select(.head_sha == \"${commit}\")) | .[0]) // empty | [.id, .head_sha, .status, (.conclusion // \"\"), .html_url] | map(tostring) | join(\"|\")"
)"
if [[ -z "$run_data" ]]; then
  echo "error: no main-branch CI push run exists for exact commit ${commit}" >&2
  echo "hint: wait for CI to start and finish, then retry the release" >&2
  exit 1
fi

IFS='|' read -r run_id run_head_sha run_status run_conclusion run_url <<<"$run_data"
if [[ "$run_head_sha" != "$commit" ]]; then
  echo "error: CI run ${run_id:-unknown} is for ${run_head_sha:-unknown}, not ${commit}" >&2
  exit 1
fi
if [[ "$run_status" != "completed" ]]; then
  echo "error: CI run ${run_id:-unknown} for ${commit} is still ${run_status:-unknown}" >&2
  [[ -n "$run_url" ]] && echo "run: ${run_url}" >&2
  exit 1
fi
if [[ "$run_conclusion" != "success" ]]; then
  echo "error: CI run ${run_id:-unknown} for ${commit} concluded ${run_conclusion:-unknown}" >&2
  [[ -n "$run_url" ]] && echo "run: ${run_url}" >&2
  exit 1
fi

echo "Verified exact-SHA CI run ${run_id} for ${commit}"
