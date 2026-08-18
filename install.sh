#!/usr/bin/env bash
set -euo pipefail

REPO="openclaw/ocm"
DEFAULT_PREFIX="${HOME}/.local"

usage() {
  cat <<'EOF'
Install ocm from GitHub release binaries.

Usage:
  install.sh [--version <tag>] [--prefix <dir>] [--bin-dir <dir>] [--force]

Options:
  --version <tag>  Release tag to install. Defaults to the latest release.
  --prefix <dir>   Base install prefix. Defaults to ~/.local.
  --bin-dir <dir>  Install directory for the ocm binary. Defaults to <prefix>/bin.
  --force          Overwrite an existing ocm binary.
  --help           Show this help.
EOF
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: required command not found: $1" >&2
    exit 1
  fi
}

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Darwin) os="apple-darwin" ;;
    Linux) os="unknown-linux-gnu" ;;
    *)
      echo "error: unsupported operating system: $os" >&2
      exit 1
      ;;
  esac

  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *)
      echo "error: unsupported architecture: $arch" >&2
      exit 1
      ;;
  esac

  if [[ "$os" == "unknown-linux-gnu" && "$arch" == "aarch64" ]]; then
    echo "error: unsupported platform: aarch64-unknown-linux-gnu" >&2
    exit 1
  fi

  printf '%s-%s\n' "$arch" "$os"
}

download_url_for() {
  local version="$1"
  local asset="$2"
  if [[ "$version" == "latest" ]]; then
    printf 'https://github.com/%s/releases/latest/download/%s\n' "$REPO" "$asset"
  else
    printf 'https://github.com/%s/releases/download/%s/%s\n' "$REPO" "$version" "$asset"
  fi
}

version="latest"
prefix="$DEFAULT_PREFIX"
bin_dir=""
force="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      shift
      [[ $# -gt 0 ]] || { echo "error: --version requires a value" >&2; exit 1; }
      version="$1"
      ;;
    --prefix)
      shift
      [[ $# -gt 0 ]] || { echo "error: --prefix requires a value" >&2; exit 1; }
      prefix="$1"
      ;;
    --bin-dir)
      shift
      [[ $# -gt 0 ]] || { echo "error: --bin-dir requires a value" >&2; exit 1; }
      bin_dir="$1"
      ;;
    --force)
      force="true"
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

require_command curl
require_command tar
require_command mktemp
require_command uname
require_command awk
require_command tr
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
  echo "error: required command not found: sha256sum or shasum" >&2
  exit 1
fi

if [[ -z "$bin_dir" ]]; then
  bin_dir="${prefix}/bin"
fi

target="$(detect_target)"
asset="ocm-${target}.tar.gz"
url="$(download_url_for "$version" "$asset")"
checksums_url="$(download_url_for "$version" "SHA256SUMS")"

tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

archive_path="${tmp_dir}/${asset}"
checksums_path="${tmp_dir}/SHA256SUMS"

echo "Downloading ${url}"
curl -fsSL "$url" -o "$archive_path"
curl -fsSL "$checksums_url" -o "$checksums_path"

expected_sha256="$(
  awk -v asset="$asset" '
    $2 == asset || $2 == "*" asset {
      print $1
      found = 1
      exit
    }
    END {
      if (!found) exit 1
    }
  ' "$checksums_path"
)" || {
  echo "error: SHA256SUMS does not contain ${asset}" >&2
  exit 1
}
if [[ ! "$expected_sha256" =~ ^[0-9a-fA-F]{64}$ ]]; then
  echo "error: SHA256SUMS contains an invalid digest for ${asset}" >&2
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  actual_sha256="$(sha256sum "$archive_path" | awk '{print $1}')"
else
  actual_sha256="$(shasum -a 256 "$archive_path" | awk '{print $1}')"
fi
actual_sha256="$(printf '%s' "$actual_sha256" | tr '[:upper:]' '[:lower:]')"
expected_sha256="$(printf '%s' "$expected_sha256" | tr '[:upper:]' '[:lower:]')"
if [[ "$actual_sha256" != "$expected_sha256" ]]; then
  echo "error: checksum mismatch for ${asset}" >&2
  exit 1
fi

tar -xzf "$archive_path" -C "$tmp_dir"

binary_path="${tmp_dir}/ocm"
if [[ ! -f "$binary_path" || -L "$binary_path" ]]; then
  echo "error: release archive did not contain a regular ocm binary" >&2
  exit 1
fi

mkdir -p "$bin_dir"
destination="${bin_dir}/ocm"
if [[ -e "$destination" && "$force" != "true" ]]; then
  echo "error: ${destination} already exists; run \"ocm self update\" or rerun with --force to replace it" >&2
  exit 1
fi

install -m 0755 "$binary_path" "$destination"

echo "Installed ocm to ${destination}"
case ":${PATH}:" in
  *":${bin_dir}:"*) ;;
  *)
    echo "Add ${bin_dir} to your PATH to run ocm from any shell."
    ;;
esac
