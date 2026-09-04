#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Package a compiled ocm binary into a GitHub release archive.

Usage:
  scripts/package-release.sh --target <triple> --binary <path> [--output-dir <dir>]
    [--macos-team-id <id>]
EOF
}

target=""
binary=""
output_dir="dist"
macos_team_id=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      shift
      [[ $# -gt 0 ]] || { echo "error: --target requires a value" >&2; exit 1; }
      target="$1"
      ;;
    --binary)
      shift
      [[ $# -gt 0 ]] || { echo "error: --binary requires a value" >&2; exit 1; }
      binary="$1"
      ;;
    --output-dir)
      shift
      [[ $# -gt 0 ]] || { echo "error: --output-dir requires a value" >&2; exit 1; }
      output_dir="$1"
      ;;
    --macos-team-id)
      shift
      [[ $# -gt 0 ]] || { echo "error: --macos-team-id requires a value" >&2; exit 1; }
      macos_team_id="$1"
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
  shift
done

[[ -n "$target" ]] || { echo "error: --target is required" >&2; exit 1; }
[[ -n "$binary" ]] || { echo "error: --binary is required" >&2; exit 1; }
case "$target" in
  aarch64-apple-darwin|x86_64-apple-darwin|x86_64-unknown-linux-gnu) ;;
  *)
    echo "error: unsupported release target: $target" >&2
    exit 1
    ;;
esac
[[ -f "$binary" ]] || { echo "error: binary not found: $binary" >&2; exit 1; }
case "$target" in
  *-apple-darwin)
    [[ -n "$macos_team_id" ]] || {
      echo "error: --macos-team-id is required for macOS release archives" >&2
      exit 1
    }
    ;;
  *)
    [[ -z "$macos_team_id" ]] || {
      echo "error: --macos-team-id is only valid for macOS release archives" >&2
      exit 1
    }
    ;;
esac

mkdir -p "$output_dir"
tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

cp "$binary" "${tmp_dir}/ocm"
chmod 0755 "${tmp_dir}/ocm"
cp LICENSE "${tmp_dir}/LICENSE"
cp README.md "${tmp_dir}/README.md"

archive_path="${output_dir}/ocm-${target}.tar.gz"
tmp_archive="$(mktemp "${output_dir}/.ocm-${target}.XXXXXX")"
cleanup_archive() {
  rm -f "$tmp_archive"
}
trap 'cleanup_archive; cleanup' EXIT

tar -czf "$tmp_archive" -C "$tmp_dir" ocm LICENSE README.md
if [[ "$target" == *-apple-darwin ]]; then
  verify_dir="${tmp_dir}/verify"
  mkdir -p "$verify_dir"
  tar -xzf "$tmp_archive" -C "$verify_dir"
  script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
  "${script_dir}/verify-macos-release.sh" \
    --binary "${verify_dir}/ocm" \
    --team-id "$macos_team_id" \
    --require-notarization
fi
mv -f "$tmp_archive" "$archive_path"

echo "$archive_path"
