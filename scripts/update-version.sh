#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Update the ocm package version safely.

Usage:
  scripts/update-version.sh <version>

Examples:
  scripts/update-version.sh 1.2.3
  scripts/update-version.sh 1.0.0-beta.1
EOF
}

if [[ $# -ne 1 ]]; then
  usage >&2
  exit 1
fi

new_version="$1"

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"
cd "$repo_root"

"${script_dir}/validate-version.sh" "$new_version"

current_version="$("${script_dir}/read-package-version.sh" Cargo.toml Cargo.lock)"

if [[ "$current_version" == "$new_version" ]]; then
  echo "ocm is already on ${new_version}"
  exit 0
fi

export OCM_NEW_VERSION="$new_version"

echo "Updating Cargo.toml and Cargo.lock from ${current_version} to ${new_version}" >&2

backup_dir="$(mktemp -d)"
cp Cargo.toml "${backup_dir}/Cargo.toml"
cp Cargo.lock "${backup_dir}/Cargo.lock"
committed=0
rollback() {
  local status=$?
  if [[ "$committed" -ne 1 ]]; then
    cp "${backup_dir}/Cargo.toml" Cargo.toml
    cp "${backup_dir}/Cargo.lock" Cargo.lock
  fi
  rm -rf "$backup_dir"
  exit "$status"
}
trap rollback EXIT

perl -0pi -e 's/^(version = ")[^"]+(")/$1.$ENV{OCM_NEW_VERSION}.$2/me' Cargo.toml

perl -0pi -e 's/(\[\[package\]\]\nname = "ocm"\nversion = ")[^"]+(")/$1.$ENV{OCM_NEW_VERSION}.$2/se' Cargo.lock

updated_version="$("${script_dir}/read-package-version.sh" Cargo.toml Cargo.lock)"
if [[ "$updated_version" != "$new_version" ]]; then
  echo "error: version files did not update cleanly" >&2
  exit 1
fi

committed=1
rm -rf "$backup_dir"
trap - EXIT

echo "Updated ocm version: ${current_version} -> ${new_version}"
