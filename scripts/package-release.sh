#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Package a compiled ocm binary into a GitHub release archive.

Usage:
  scripts/package-release.sh --target <triple> --binary <path> [--output-dir <dir>]
EOF
}

target=""
binary=""
output_dir="dist"

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
mv -f "$tmp_archive" "$archive_path"

echo "$archive_path"
