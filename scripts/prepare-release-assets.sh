#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: scripts/prepare-release-assets.sh <asset-dir>" >&2
}

if [[ $# -ne 1 ]]; then
  usage
  exit 1
fi

asset_dir="$1"
expected_archives=(
  "ocm-aarch64-apple-darwin.tar.gz"
  "ocm-x86_64-apple-darwin.tar.gz"
  "ocm-x86_64-unknown-linux-gnu.tar.gz"
)
expected_members="$(printf '%s\n' "LICENSE" "README.md" "ocm")"
install_script="${asset_dir}/install.sh"

if [[ ! -f "$install_script" || -L "$install_script" ]]; then
  echo "error: required release installer is missing or invalid: install.sh" >&2
  exit 1
fi

for archive in "${expected_archives[@]}"; do
  path="${asset_dir}/${archive}"
  if [[ ! -f "$path" || -L "$path" ]]; then
    echo "error: required release archive is missing or invalid: $archive" >&2
    exit 1
  fi
  if ! tar -tzf "$path" >/dev/null; then
    echo "error: release archive is not a valid tarball: $archive" >&2
    exit 1
  fi
  actual_members="$(tar -tzf "$path" | LC_ALL=C sort)"
  if [[ "$actual_members" != "$expected_members" ]]; then
    echo "error: release archive has invalid members: $archive" >&2
    printf 'expected:\n%s\nactual:\n%s\n' "$expected_members" "$actual_members" >&2
    exit 1
  fi
  if ! tar -tvzf "$path" | awk '
    BEGIN { count = 0 }
    {
      count += 1
      if (substr($0, 1, 1) != "-") {
        exit 1
      }
    }
    END {
      if (count != 3) {
        exit 1
      }
    }
  '; then
    echo "error: release archive members must be regular files: $archive" >&2
    exit 1
  fi
done

expected_archive_list="$(printf '%s\n' "${expected_archives[@]}")"
actual_archive_list="$(
  find "$asset_dir" -maxdepth 1 -name 'ocm-*.tar.gz' -exec basename {} \; | sort
)"
if [[ "$actual_archive_list" != "$expected_archive_list" ]]; then
  echo "error: release archive set does not match the supported target matrix" >&2
  printf 'expected:\n%s\nactual:\n%s\n' "$expected_archive_list" "$actual_archive_list" >&2
  exit 1
fi

checksum_path="${asset_dir}/SHA256SUMS"
tmp_checksum="$(mktemp "${asset_dir}/.SHA256SUMS.XXXXXX")"
cleanup() {
  rm -f "$tmp_checksum"
}
trap cleanup EXIT

for archive in "${expected_archives[@]}"; do
  if command -v sha256sum >/dev/null 2>&1; then
    digest="$(sha256sum "${asset_dir}/${archive}" | awk '{print $1}')"
  else
    digest="$(shasum -a 256 "${asset_dir}/${archive}" | awk '{print $1}')"
  fi
  printf '%s  %s\n' "$digest" "$archive" >>"$tmp_checksum"
done
if command -v sha256sum >/dev/null 2>&1; then
  installer_digest="$(sha256sum "$install_script" | awk '{print $1}')"
else
  installer_digest="$(shasum -a 256 "$install_script" | awk '{print $1}')"
fi
printf '%s  install.sh\n' "$installer_digest" >>"$tmp_checksum"

mv -f "$tmp_checksum" "$checksum_path"
echo "$checksum_path"
