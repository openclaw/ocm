#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: scripts/verify-release-tag.sh --repo <owner/repo> --tag <tag> [--commit <sha>]" >&2
}

github() {
  if [[ -n "${OCM_GH_BIN:-}" ]]; then
    "$OCM_GH_BIN" "$@"
  elif command -v ghx >/dev/null 2>&1; then
    ghx --no-cache "$@"
  elif command -v gh >/dev/null 2>&1; then
    gh "$@"
  else
    echo "error: ghx or gh is required to verify release tags" >&2
    return 1
  fi
}

is_sha() {
  [[ "$1" =~ ^[0-9a-fA-F]{40}$ ]]
}

is_branch() {
  [[ -n "$1" && "$1" != *[[:space:]]* ]] &&
    git check-ref-format --branch "$1" >/dev/null 2>&1
}

repo=""
tag=""
expected_commit=""
canonical_repo="openclaw/ocm"
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
    --commit)
      shift
      [[ $# -gt 0 ]] || { echo "error: --commit requires a value" >&2; exit 1; }
      expected_commit="$1"
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
  shift
done

[[ -n "$repo" && -n "$tag" ]] || { usage; exit 1; }
if [[ "$repo" != "$canonical_repo" ]]; then
  echo "error: release tag repository must be ${canonical_repo}: ${repo}" >&2
  exit 1
fi
if [[ "$tag" != v* || "$tag" == "v" ]]; then
  echo "error: release tag must be a v-prefixed semantic version: ${tag}" >&2
  exit 1
fi
version="${tag#v}"

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"
"${script_dir}/validate-version.sh" "$version"

if [[ -n "$expected_commit" ]] && ! is_sha "$expected_commit"; then
  echo "error: invalid release commit SHA: $expected_commit" >&2
  exit 1
fi

ref_data="$(
  github api "repos/${repo}/git/ref/tags/${tag}" \
    --jq '[.object.type, .object.sha] | @tsv'
)"
IFS=$'\t' read -r ref_type tag_object_sha ref_extra <<<"$ref_data"
if [[ -n "${ref_extra:-}" || "$ref_type" != "tag" ]] || ! is_sha "$tag_object_sha"; then
  echo "error: release tag ${tag} must be an annotated tag with a valid object" >&2
  exit 1
fi

tag_data="$(
  github api "repos/${repo}/git/tags/${tag_object_sha}" \
    --jq '[.tag, .object.type, .object.sha, (.verification.verified | tostring)] | @tsv'
)"
IFS=$'\t' read -r verified_tag target_type target_sha signature_verified tag_extra <<<"$tag_data"
if [[ -n "${tag_extra:-}" ||
  "$verified_tag" != "$tag" ||
  "$target_type" != "commit" ]] ||
  ! is_sha "$target_sha"; then
  echo "error: release tag ${tag} does not identify one valid commit" >&2
  exit 1
fi
if [[ "$signature_verified" != "true" ]]; then
  echo "error: GitHub did not verify the signature for release tag ${tag}" >&2
  exit 1
fi
if [[ -n "$expected_commit" && "$target_sha" != "$expected_commit" ]]; then
  echo "error: release tag ${tag} moved away from verified commit ${expected_commit}" >&2
  exit 1
fi

default_branch="$(github api "repos/${repo}" --jq '.default_branch')"
if [[ "$default_branch" != "main" ]] || ! is_branch "$default_branch"; then
  echo "error: invalid protected default branch: ${default_branch:-empty}" >&2
  exit 1
fi
default_head="$(
  github api "repos/${repo}/git/ref/heads/${default_branch}" --jq '.object.sha'
)"
if ! is_sha "$default_head"; then
  echo "error: invalid protected branch head SHA" >&2
  exit 1
fi
compare_status="$(
  github api "repos/${repo}/compare/${target_sha}...${default_head}" --jq '.status'
)"
if [[ "$compare_status" != "ahead" && "$compare_status" != "identical" ]]; then
  echo "error: release commit ${target_sha} is not on protected ${default_branch}" >&2
  exit 1
fi

if ! git -C "$repo_root" cat-file -e "${target_sha}^{commit}" 2>/dev/null; then
  echo "error: verified release commit is not available locally: ${target_sha}" >&2
  exit 1
fi

target_dir="$(mktemp -d "${TMPDIR:-/tmp}/ocm-release-target.XXXXXX")"
cleanup() {
  rm -rf "$target_dir"
}
trap cleanup EXIT
git -C "$repo_root" show "${target_sha}:Cargo.toml" >"${target_dir}/Cargo.toml"
git -C "$repo_root" show "${target_sha}:Cargo.lock" >"${target_dir}/Cargo.lock"
package_version="$(
  "${script_dir}/read-package-version.sh" \
    "${target_dir}/Cargo.toml" \
    "${target_dir}/Cargo.lock"
)"
if [[ "$package_version" != "$version" ]]; then
  echo "error: release tag ${tag} does not match package version ${package_version}" >&2
  exit 1
fi

parent_data="$(git -C "$repo_root" rev-list --parents -n1 "$target_sha")"
read -r commit_sha parent_sha parent_extra <<<"$parent_data"
if [[ -n "${parent_extra:-}" ||
  "$commit_sha" != "$target_sha" ]] ||
  ! is_sha "$parent_sha"; then
  echo "error: release commit ${target_sha} must have exactly one parent" >&2
  exit 1
fi
changed_files="$(
  git -C "$repo_root" diff-tree \
    --no-commit-id \
    --name-only \
    -r \
    "$parent_sha" \
    "$target_sha" |
    LC_ALL=C sort -u
)"
if [[ "$changed_files" != $'Cargo.lock\nCargo.toml' ]]; then
  echo "error: release commit ${target_sha} must change only Cargo.toml and Cargo.lock" >&2
  exit 1
fi

subject="$(git -C "$repo_root" log -1 --format=%s "$target_sha")"
subject_prefix="chore(release): bump version to ${version} (#"
if [[ "$subject" != "${subject_prefix}"*")" ]]; then
  echo "error: release commit has invalid subject: ${subject}" >&2
  exit 1
fi
pr_number="${subject#"$subject_prefix"}"
pr_number="${pr_number%)}"
if [[ ! "$pr_number" =~ ^[1-9][0-9]*$ ||
  "$subject" != "${subject_prefix}${pr_number})" ]]; then
  echo "error: release commit subject has invalid pull request number" >&2
  exit 1
fi

pr_data="$(
  github api \
    -H 'Accept: application/vnd.github+json' \
    "repos/${repo}/commits/${target_sha}/pulls" \
    --jq '.[] | [.number, .state, (.merged_at // ""), .base.ref, .head.ref, .merge_commit_sha, .title] | @tsv'
)"
pr_line_count="$(printf '%s\n' "$pr_data" | awk 'NF { count += 1 } END { print count + 0 }')"
if [[ "$pr_line_count" != "1" ]]; then
  echo "error: release commit must be associated with exactly one pull request" >&2
  exit 1
fi
IFS=$'\t' read -r api_pr_number pr_state merged_at base_branch head_branch merge_commit pr_title pr_extra <<<"$pr_data"
if [[ -n "${pr_extra:-}" ||
  ! "$api_pr_number" =~ ^[1-9][0-9]*$ ||
  "$api_pr_number" != "$pr_number" ||
  "$pr_state" != "closed" ||
  ! "$merged_at" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] ||
  ! is_branch "$base_branch" ||
  ! is_branch "$head_branch" ||
  ! is_sha "$merge_commit"; then
  echo "error: release pull request metadata is invalid" >&2
  exit 1
fi
if [[ "$base_branch" != "$default_branch" ||
  "$head_branch" != "release/${tag}" ||
  "$merge_commit" != "$target_sha" ||
  "$pr_title" != "chore(release): bump version to ${version}" ]]; then
  echo "error: release pull request does not match the release commit" >&2
  exit 1
fi

printf '%s\n' "$target_sha"
