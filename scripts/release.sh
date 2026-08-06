#!/usr/bin/env bash
set -euo pipefail

timestamp() {
  date '+%H:%M:%S'
}

log_step() {
  printf '[%s] %s\n' "$(timestamp)" "$*" >&2
}

run_step() {
  local description="$1"
  shift
  local started_at="$SECONDS"
  log_step "$description"
  "$@"
  log_step "done: ${description} ($((SECONDS - started_at))s)"
}

usage() {
  cat <<'EOF'
Prepare or finish an ocm release through a pull request.

Usage:
  scripts/release.sh <version> [--remote <name>] [--skip-checks]

Run the same command twice:
  1. From current main, create the release branch and pull request.
  2. After squash-merging that pull request, update main and rerun to sign and push the tag.
EOF
}

package_version() {
  perl -ne 'print "$1\n" if /^version = "([^"]+)"$/' Cargo.toml | head -n1
}

lockfile_version() {
  perl -0ne 'print "$1\n" if /\[\[package\]\]\nname = "ocm"\nversion = "([^"]+)"/s' Cargo.lock | head -n1
}

version_at_ref() {
  git show "$1:Cargo.toml" |
    perl -ne 'print "$1\n" if /^version = "([^"]+)"$/' |
    head -n1
}

remote_ref_commit() {
  git ls-remote "$remote" "$1" | awk 'NR == 1 { print $1 }'
}

remote_tag_commit() {
  git ls-remote "$remote" "refs/tags/${tag}^{}" "refs/tags/${tag}" | awk '
    $2 ~ /\^\{\}$/ { print $1; found=1; exit }
    NR == 1 { first=$1 }
    END { if (!found && first != "") print first }
  '
}

remote_tag_object() {
  git ls-remote "$remote" "refs/tags/${tag}" | awk 'NR == 1 { print $1 }'
}

tag_has_signature() {
  git cat-file -p "$1" | grep -Eq -- '-----BEGIN (PGP|SSH) SIGNATURE-----'
}

ssh_public_signing_key() {
  local signing_key key_path candidate first_field
  signing_key="$(git config --get user.signingkey || true)"
  case "$signing_key" in
    key::*)
      printf '%s\n' "${signing_key#key::}"
      return 0
      ;;
    ssh-*|ecdsa-*|sk-*)
      printf '%s\n' "$signing_key"
      return 0
      ;;
  esac

  key_path="$(git config --path --get user.signingkey || true)"
  [[ -n "$key_path" ]] || return 1
  for candidate in "$key_path" "${key_path}.pub"; do
    [[ -f "$candidate" ]] || continue
    read -r first_field _ <"$candidate" || continue
    case "$first_field" in
      ssh-*|ecdsa-*|sk-*)
        head -n1 "$candidate"
        return 0
        ;;
    esac
  done
  return 1
}

verify_tag_signature() {
  local release_tag="$1"
  local signing_format allowed_signers principal public_key trust_dir trust_file verify_status
  signing_format="$(git config --get gpg.format || true)"
  if [[ "$signing_format" != "ssh" ]]; then
    git verify-tag "$release_tag"
    return
  fi

  allowed_signers="$(git config --path --get gpg.ssh.allowedSignersFile || true)"
  if [[ -n "$allowed_signers" ]]; then
    git verify-tag "$release_tag"
    return
  fi

  principal="$(git config --get user.email || true)"
  [[ -n "$principal" && "$principal" != *[[:space:],]* ]] || return 1
  public_key="$(ssh_public_signing_key)" || return 1
  trust_dir="$(mktemp -d "${TMPDIR:-/tmp}/ocm-release-signers.XXXXXX")" || return 1
  trust_file="${trust_dir}/allowed_signers"
  chmod 700 "$trust_dir"
  printf '%s namespaces="git" %s\n' "$principal" "$public_key" >"$trust_file"
  chmod 600 "$trust_file"

  if git -c "gpg.ssh.allowedSignersFile=$trust_file" verify-tag "$release_tag"; then
    verify_status=0
  else
    verify_status=$?
  fi
  rm -rf "$trust_dir"
  return "$verify_status"
}

tracked_dirty_files() {
  {
    git diff --name-only --ignore-submodules --
    git diff --cached --name-only --ignore-submodules --
  } | sort -u
}

require_clean_checkout() {
  local dirty
  dirty="$(tracked_dirty_files)"
  if [[ -n "$dirty" ]]; then
    echo "error: tracked changes are present; commit or stash them before running scripts/release.sh" >&2
    printf '%s\n' "$dirty" >&2
    exit 1
  fi
}

release_commit_is_version_only() {
  local changed
  changed="$(git diff-tree --no-commit-id --name-only -r HEAD | sort -u)"
  [[ "$changed" == $'Cargo.lock\nCargo.toml' || "$changed" == $'Cargo.toml\nCargo.lock' ]]
}

release_commit_subject_matches() {
  local subject="$1"
  local suffix
  if [[ "$subject" == "$release_commit_message" ]]; then
    return 0
  fi
  suffix="${subject#"$release_commit_message"}"
  [[ "$suffix" =~ ^\ \(#[0-9]+\)$ ]]
}

github_cli() {
  if [[ -n "${OCM_GH_BIN:-}" ]]; then
    printf '%s\n' "$OCM_GH_BIN"
  elif command -v ghx >/dev/null 2>&1; then
    command -v ghx
  elif command -v gh >/dev/null 2>&1; then
    command -v gh
  else
    echo "error: ghx or gh is required to create the release pull request" >&2
    exit 1
  fi
}

github_repository() {
  local remote_url repository
  if [[ -n "${OCM_GITHUB_REPOSITORY:-}" ]]; then
    repository="$OCM_GITHUB_REPOSITORY"
  else
    remote_url="$(git remote get-url "$remote")"
    case "$remote_url" in
      https://github.com/*)
        repository="${remote_url#https://github.com/}"
        ;;
      git@github.com:*)
        repository="${remote_url#git@github.com:}"
        ;;
      ssh://git@github.com/*)
        repository="${remote_url#ssh://git@github.com/}"
        ;;
      *)
        echo "error: cannot derive a GitHub repository from remote ${remote}; set OCM_GITHUB_REPOSITORY" >&2
        return 1
        ;;
    esac
    repository="${repository%.git}"
  fi
  if [[ ! "$repository" =~ ^[^/[:space:]]+/[^/[:space:]]+$ ]]; then
    echo "error: invalid GitHub repository: ${repository}" >&2
    return 1
  fi
  printf '%s\n' "$repository"
}

ensure_release_pr() {
  local gh_bin pr_url repository
  gh_bin="$(github_cli)"
  repository="$(github_repository)"
  pr_url="$(
    "$gh_bin" pr view "$release_branch" \
      --repo "$repository" \
      --json url,state,baseRefName \
      --jq 'select(.state == "OPEN" and .baseRefName == "main") | .url' \
      2>/dev/null || true
  )"
  if [[ -z "$pr_url" ]]; then
    if ! pr_url="$(
      "$gh_bin" pr create \
        --repo "$repository" \
        --base main \
        --head "$release_branch" \
        --title "$release_commit_message" \
        --body "Prepare ${tag}. Squash-merge this pull request, update local main, then rerun \`scripts/release.sh ${version}\` to sign and push the release tag."
    )" || [[ -z "$pr_url" ]]; then
      echo "error: failed to create the release pull request" >&2
      return 1
    fi
  fi
  printf '%s\n' "$pr_url"
}

verify_existing_remote_tag() {
  local remote_main_sha remote_tag_commit_sha remote_tag_object_sha local_tag_object_sha tagged_version
  remote_tag_commit_sha="$(remote_tag_commit)"
  [[ -n "$remote_tag_commit_sha" ]] || return 1
  remote_tag_object_sha="$(remote_tag_object)"

  if git show-ref --verify --quiet "refs/tags/${tag}"; then
    local_tag_object_sha="$(git rev-parse "${tag}^{tag}" 2>/dev/null || true)"
    if [[ -z "$local_tag_object_sha" || "$local_tag_object_sha" != "$remote_tag_object_sha" ]]; then
      echo "error: local tag ${tag} differs from the remote tag" >&2
      exit 1
    fi
  else
    run_step "Fetching existing tag ${tag}" git fetch "$remote" "refs/tags/${tag}:refs/tags/${tag}"
  fi

  if [[ "$(git cat-file -t "$tag" 2>/dev/null || true)" != "tag" ]] ||
    ! tag_has_signature "$tag" ||
    ! verify_tag_signature "$tag" >/dev/null 2>&1; then
    echo "error: remote tag ${tag} is not a valid signed annotated tag" >&2
    exit 1
  fi
  tagged_version="$(version_at_ref "$tag")"
  if [[ "$tagged_version" != "$version" ]]; then
    echo "error: remote tag ${tag} contains package version ${tagged_version:-unknown}" >&2
    exit 1
  fi

  run_step "Refreshing ${remote}/main" git fetch "$remote" main
  remote_main_sha="$(remote_ref_commit "refs/heads/main")"
  if ! git merge-base --is-ancestor "$remote_tag_commit_sha" "$remote_main_sha"; then
    echo "error: remote tag ${tag} is not reachable from ${remote}/main" >&2
    exit 1
  fi

  cat <<EOF
Release tag ${tag} is already published from verified commit ${remote_tag_commit_sha}.
The tag-push workflow owns artifact publication.
EOF
}

version=""
remote="origin"
skip_checks=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --remote)
      shift
      [[ $# -gt 0 ]] || {
        echo "error: --remote requires a value" >&2
        exit 1
      }
      remote="$1"
      ;;
    --skip-checks)
      skip_checks=1
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    -*)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
    *)
      if [[ -n "$version" ]]; then
        echo "error: version was already provided: $version" >&2
        exit 1
      fi
      version="$1"
      ;;
  esac
  shift
done

if [[ -z "$version" ]]; then
  usage >&2
  exit 1
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"
cd "$repo_root"

"${script_dir}/validate-version.sh" "$version"

if ! git remote get-url "$remote" >/dev/null 2>&1; then
  echo "error: git remote not found: $remote" >&2
  exit 1
fi

tag="v${version}"
release_branch="release/${tag}"
release_commit_message="chore(release): bump version to ${version}"
branch="$(git symbolic-ref --quiet --short HEAD || true)"

if verify_existing_remote_tag; then
  exit 0
fi

current_version="$(package_version)"
current_lock_version="$(lockfile_version)"
if [[ -z "$current_version" || -z "$current_lock_version" ]]; then
  echo "error: could not read the ocm version from Cargo.toml and Cargo.lock" >&2
  exit 1
fi
if [[ "$current_version" != "$current_lock_version" ]]; then
  echo "error: Cargo.toml and Cargo.lock are out of sync" >&2
  exit 1
fi

case "$branch" in
  main)
    require_clean_checkout
    remote_main_sha="$(remote_ref_commit "refs/heads/main")"
    head_sha="$(git rev-parse HEAD)"
    if [[ -z "$remote_main_sha" || "$head_sha" != "$remote_main_sha" ]]; then
      echo "error: local main must exactly match ${remote}/main before release work" >&2
      exit 1
    fi

    if [[ "$current_version" == "$version" ]]; then
      if ! release_commit_subject_matches "$(git log -1 --pretty=%s)" ||
        ! release_commit_is_version_only; then
        echo "error: ${version} is on main, but HEAD is not the expected squash-merged release commit" >&2
        exit 1
      fi
      run_step "Verifying exact-SHA main CI" \
        "${script_dir}/verify-release-ci.sh" \
        --repo "$(github_repository)" \
        --commit "$head_sha"

      if git show-ref --verify --quiet "refs/tags/${tag}"; then
        log_step "Using existing local tag ${tag}"
      else
        log_step "Creating signed tag ${tag}; git signing may prompt here"
        git -c tag.gpgSign=true tag -a "$tag" -m "$tag"
      fi
      if [[ "$(git rev-list -n1 "$tag")" != "$head_sha" ]]; then
        echo "error: local tag ${tag} does not point at the release commit" >&2
        exit 1
      fi
      if ! tag_has_signature "$tag" || ! verify_tag_signature "$tag" >/dev/null 2>&1; then
        echo "error: created tag ${tag} does not have a valid configured signature" >&2
        exit 1
      fi
      run_step "Pushing signed tag ${tag}" git push "$remote" "$tag"
      cat <<EOF
Release tag ${tag} pushed from $(git rev-parse HEAD).
The tag-push workflow will verify, build, and publish the release.
EOF
      exit 0
    fi

    if git show-ref --verify --quiet "refs/heads/${release_branch}"; then
      echo "error: local branch ${release_branch} already exists; switch to it and rerun" >&2
      exit 1
    fi
    run_step "Creating ${release_branch}" git switch -c "$release_branch"
    ;;
  "$release_branch")
    ;;
  *)
    echo "error: run from main or ${release_branch} (current: ${branch:-detached})" >&2
    exit 1
    ;;
esac

dirty="$(tracked_dirty_files)"
if [[ -n "$dirty" && "$dirty" != $'Cargo.lock\nCargo.toml' && "$dirty" != $'Cargo.toml\nCargo.lock' ]]; then
  echo "error: release branches may contain only Cargo.toml and Cargo.lock changes" >&2
  printf '%s\n' "$dirty" >&2
  exit 1
fi

if [[ "$current_version" != "$version" ]]; then
  if [[ -n "$dirty" ]]; then
    echo "error: version files are already dirty but do not contain ${version}" >&2
    exit 1
  fi
  run_step "Updating version files to ${version}" "${script_dir}/update-version.sh" "$version"
fi

release_commit_ready=0
if [[ "$(git log -1 --pretty=%s)" == "$release_commit_message" ]] &&
  release_commit_is_version_only &&
  [[ -z "$(tracked_dirty_files)" ]]; then
  release_commit_ready=1
fi

if [[ "$release_commit_ready" -eq 1 ]]; then
  log_step "Release commit already exists; skipping completed checks"
elif [[ "$skip_checks" -eq 0 ]]; then
  run_step "Running cargo fmt --check" cargo fmt --check
  run_step "Running test suite" cargo test --locked
  run_step "Building release binary" cargo build --locked --release
else
  log_step "Local checks are skipped"
fi

if [[ "$release_commit_ready" -eq 0 ]]; then
  run_step "Staging version files" git add Cargo.toml Cargo.lock
  if git diff --cached --quiet -- Cargo.toml Cargo.lock; then
    echo "error: no version changes remain to commit" >&2
    exit 1
  fi
  run_step "Creating release commit" git commit -m "$release_commit_message"
elif [[ -n "$(tracked_dirty_files)" ]]; then
  echo "error: release commit exists but version files are still dirty" >&2
  exit 1
fi

run_step "Pushing ${release_branch}" git push --set-upstream "$remote" "$release_branch"
pr_url="$(ensure_release_pr)"

cat <<EOF
Release pull request ready: ${pr_url}

Next:
  1. Squash-merge ${release_branch} into main.
  2. Update local main to the merged commit.
  3. Wait for the exact main-branch CI run to pass.
  4. Rerun: scripts/release.sh ${version} --remote ${remote}
EOF
