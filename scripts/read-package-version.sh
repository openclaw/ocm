#!/usr/bin/env bash
set -euo pipefail

print_name=0
if [[ "${1:-}" == "--name" ]]; then
  print_name=1
  shift
fi
if [[ $# -ne 2 ]]; then
  echo "Usage: scripts/read-package-version.sh [--name] <Cargo.toml> <Cargo.lock>" >&2
  exit 1
fi

manifest="$1"
lockfile="$2"

[[ -f "$manifest" && ! -L "$manifest" ]] || {
  echo "error: package manifest is missing or invalid: $manifest" >&2
  exit 1
}
[[ -f "$lockfile" && ! -L "$lockfile" ]] || {
  echo "error: package lockfile is missing or invalid: $lockfile" >&2
  exit 1
}

manifest_data="$(
  perl -0e '
    use strict;
    use warnings;

    local $/;
    my $content = <>;
    my @sections = split(/(?=^\[)/m, $content);
    my @package_sections = grep { /^\[package\]\s*$/m } @sections;
    die "expected exactly one [package] section\n" unless @package_sections == 1;

    my $section = $package_sections[0];
    my @names = ($section =~ /^name\s*=\s*"([^"]+)"\s*$/mg);
    my @versions = ($section =~ /^version\s*=\s*"([^"]+)"\s*$/mg);
    die "expected exactly one package name\n" unless @names == 1;
    die "package name must be ocm or openclawocm\n"
      unless $names[0] eq "ocm" || $names[0] eq "openclawocm";
    die "expected exactly one package version\n" unless @versions == 1;
    print "$names[0] $versions[0]\n";
  ' "$manifest"
)" || {
  echo "error: could not read a unique OCM package version from $manifest" >&2
  exit 1
}
read -r package_name manifest_version <<<"$manifest_data"

lock_version="$(
  OCM_PACKAGE_NAME="$package_name" perl -0e '
    use strict;
    use warnings;

    local $/;
    my $content = <>;
    my @matches;
    while ($content =~ /(?:\A|\n)\[\[package\]\]\s*\n(.*?)(?=\n\[\[package\]\]\s*\n|\z)/sg) {
      my $block = $1;
      my @names = ($block =~ /^name\s*=\s*"([^"]+)"\s*$/mg);
      next unless grep { $_ eq $ENV{OCM_PACKAGE_NAME} } @names;
      push @matches, $block;
    }
    die "expected exactly one matching package record\n" unless @matches == 1;

    my $block = $matches[0];
    my @names = ($block =~ /^name\s*=\s*"([^"]+)"\s*$/mg);
    my @versions = ($block =~ /^version\s*=\s*"([^"]+)"\s*$/mg);
    die "expected one matching package name\n" unless @names == 1 && $names[0] eq $ENV{OCM_PACKAGE_NAME};
    die "expected one package version\n" unless @versions == 1;
    die "package record must be local\n" if $block =~ /^(?:source|checksum)\s*=/m;
    print "$versions[0]\n";
  ' "$lockfile"
)" || {
  echo "error: could not read a unique local ${package_name} package version from $lockfile" >&2
  exit 1
}

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
"${script_dir}/validate-version.sh" "$manifest_version"
"${script_dir}/validate-version.sh" "$lock_version"

if [[ "$manifest_version" != "$lock_version" ]]; then
  echo "error: Cargo.toml and Cargo.lock ${package_name} versions do not match" >&2
  exit 1
fi

if [[ "$print_name" == 1 ]]; then
  printf '%s\n' "$package_name"
else
  printf '%s\n' "$manifest_version"
fi
