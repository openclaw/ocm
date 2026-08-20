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
canonical_repo="openclaw/ocm"
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

if [[ "$repo" != "$canonical_repo" ]]; then
  echo "error: release CI repository must be ${canonical_repo}: ${repo:-empty}" >&2
  exit 1
fi
if [[ ! "$commit" =~ ^[0-9a-fA-F]{40}$ ]]; then
  echo "error: invalid release commit SHA: ${commit:-empty}" >&2
  exit 1
fi

endpoint="repos/${repo}/actions/workflows/ci.yml/runs?branch=main&event=push&head_sha=${commit}&per_page=20"
run_data="$(
  github api "$endpoint" \
    --jq "(.workflow_runs // []) as \$runs | [(\$runs | length), ((\$runs[0].id // \"\") | tostring), (\$runs[0].head_sha // \"\"), (\$runs[0].event // \"\"), (\$runs[0].head_branch // \"\"), (\$runs[0].status // \"\"), (\$runs[0].conclusion // \"\"), (\$runs[0].html_url // \"\")] | join(\"|\")"
)"
if [[ -z "$run_data" ]]; then
  echo "error: no main-branch CI push run exists for exact commit ${commit}" >&2
  echo "hint: wait for CI to start and finish, then retry the release" >&2
  exit 1
fi

IFS='|' read -r run_count run_id run_head_sha run_event run_branch run_status run_conclusion run_url run_extra <<<"$run_data"
if [[ -n "${run_extra:-}" || ! "$run_count" =~ ^[0-9]+$ ]]; then
  echo "error: malformed CI run selection response" >&2
  exit 1
fi
if [[ "$run_count" == "0" ]]; then
  echo "error: no main-branch CI push run exists for exact commit ${commit}" >&2
  echo "hint: wait for CI to start and finish, then retry the release" >&2
  exit 1
fi
if [[ "$run_count" != "1" ]]; then
  echo "error: expected exactly one main-branch CI push run for ${commit}, found ${run_count}" >&2
  exit 1
fi
if [[ ! "$run_id" =~ ^[0-9]+$ ||
  ! "$run_head_sha" =~ ^[0-9a-fA-F]{40}$ ||
  "$run_event" != "push" ||
  "$run_branch" != "main" ||
  ! "$run_status" =~ ^(queued|in_progress|completed)$ ||
  ! "$run_conclusion" =~ ^(success|failure|cancelled|skipped|timed_out|action_required|neutral|stale|startup_failure)?$ ||
  ! "$run_url" =~ ^https://github\.com/openclaw/ocm/actions/runs/[0-9]+$ ]]; then
  echo "error: malformed or unrelated CI run response" >&2
  exit 1
fi
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
